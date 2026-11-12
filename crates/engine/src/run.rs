//! One download's life: probe, resume, schedule connections, verify,
//! finish. One `Driver` per running download, on its own task; each
//! connection is a further task (`worker`).
//!
//! The scheduler loop reacts at once to a connection finishing (it takes
//! over the largest remaining range of the slowest connection) and, every
//! tick, samples throughput, tunes the connection count, hedges stalled
//! connections and checkpoints (`Storage::flush`, then `Journal::commit`).

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{StreamExt, stream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use vayu_core::{
    ByteRange, ByteStream, Checkpoint, DownloadRecord, Finishing, HostProfile, OpenRequest,
    RangeSet, RangeSource, RemoteMeta, SourceError, Storage, Validators,
};
use vayu_proto::{
    Checksum, DownloadId, DownloadState, Durability, ErrorInfo, RequestContext, SegmentInfo,
};

use crate::checksum;
use crate::config::Tuning;
use crate::engine::{Inner, Live};
use crate::names;
use crate::segment::{HedgeKind, Segment, WorkerId};
use crate::tuner::{Sample, Tuner};
use crate::util::{backoff, host_key, median, unix_millis};
use crate::worker::{self, Counters, Exit, Job};

/// Why the service stopped a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    Pause,
    Shutdown,
    Remove,
    /// `refresh_url` on a running download: stop, then start again.
    Restart,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed { filename: String, total: u64 },
    Failed(ErrorInfo),
    NeedsRefresh(ErrorInfo),
    Stopped(StopReason),
}

/// What a start found of a download that had finished downloading.
enum Recovery {
    None,
    /// The part file holds every byte: verify and finish.
    FinishAgain,
    /// The final file is in place: completed.
    Adopted,
}

/// The interim intent marker (before `Checkpoint::finishing` existed):
/// `RemoteMeta::final_url` = this prefix + the name. Still read, never
/// written.
const LEGACY_FINISHING: &str = "vayu-finishing:";

/// The finish intent of a checkpoint: the field, or the interim marker.
fn intent(cp: &Checkpoint) -> Option<Finishing> {
    cp.finishing.clone().or_else(|| {
        let name = cp
            .meta
            .final_url
            .as_deref()?
            .strip_prefix(LEGACY_FINISHING)?;
        Some(Finishing {
            name: name.to_string(),
            len: cp.meta.size?,
        })
    })
}

/// `meta` without an interim intent marker.
fn without_intent(mut meta: RemoteMeta) -> RemoteMeta {
    if meta
        .final_url
        .as_deref()
        .is_some_and(|u| u.starts_with(LEGACY_FINISHING))
    {
        meta.final_url = None;
    }
    meta
}

/// Why the transfer left its loop early.
enum Abort {
    /// `RemoteChanged`: restart from zero (once).
    Remote,
    /// `RangesUnsupported`: start again on one connection from zero.
    NoRanges,
    Out(Outcome),
}

impl From<Outcome> for Abort {
    fn from(o: Outcome) -> Self {
        Abort::Out(o)
    }
}

enum Plan {
    Ranged(RangeSet),
    Stream,
}

enum Work {
    Own(Arc<Segment>),
    Hedge(Arc<Segment>, HedgeKind),
}

/// The response `RangeSource::start` began, and how long `start` took (its
/// time to first byte).
type First = (ByteStream, Duration);

/// A fresh download's first range, downloaded by one connection alone
/// until splitting it pays (`Driver::solo_check`): the response `start`
/// began, or, if that broke, the one connection that took over.
#[derive(Debug, Clone)]
struct Solo {
    seg: Arc<Segment>,
    /// How long a new connection takes to its first byte, seconds: what
    /// `start` took.
    ttfb: f64,
    next: Instant,
    /// The previous look at the connection with bytes: (connection, when,
    /// bytes received). Rates are taken from here on, past the burst a
    /// server (or the socket buffers) delivers first.
    mark: Option<(WorkerId, Instant, u64)>,
}

/// Why a solo run ended.
enum SoloEnd {
    Hold,
    /// Gone quiet, cancelled, or no owner to judge.
    Other,
    /// Its time to finish exceeds what another connection is worth; at
    /// this rate (bytes/s, past the first burst).
    Eta(f64),
}

pub(crate) fn error_info(code: &str, message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        code: code.into(),
        message: message.into(),
    }
}

fn source_info(e: &SourceError) -> ErrorInfo {
    error_info(e.code(), e.to_string())
}

pub(crate) fn io_info(e: &io::Error) -> ErrorInfo {
    let code = if e.kind() == io::ErrorKind::StorageFull {
        "disk_full"
    } else {
        "io"
    };
    error_info(code, e.to_string())
}

fn io_failed(e: io::Error) -> Outcome {
    Outcome::Failed(io_info(&e))
}

/// One connection, as the scheduler sees it.
struct Wk {
    seg: Arc<Segment>,
    counters: Arc<Counters>,
    cancel: CancellationToken,
    started: Instant,
    cancelling: bool,
    last_bytes: u64,
    ewma: Option<f64>,
    ttfb_seen: bool,
}

impl Wk {
    fn first_byte(&self) -> bool {
        self.counters.last_us.load(Ordering::Relaxed) != 0
    }
}

/// Everything a run needs from its download record.
pub(crate) struct RunSpec {
    pub record: DownloadRecord,
    pub generation: u64,
    pub live: Arc<Live>,
    /// The user named the file (`filename_locked`): probes never rename it.
    pub name_locked: bool,
    /// `AddParams::connections`.
    pub connections: Option<u16>,
}

/// Runs a download and makes sure it always ends: a panic anywhere in the
/// run still applies a final state (`failed`, code `internal`) and frees
/// the queue slot, instead of leaving the download marked running forever.
pub(crate) async fn supervise(
    inner: Arc<Inner>,
    spec: RunSpec,
    stop: watch::Receiver<Option<StopReason>>,
    done: watch::Sender<bool>,
) {
    use futures::FutureExt;
    let id = spec.record.id.clone();
    let generation = spec.generation;
    let ran = std::panic::AssertUnwindSafe(run(inner.clone(), spec, stop))
        .catch_unwind()
        .await;
    if let Err(panic) = ran {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        tracing::error!(id, "download run panicked: {what}");
        inner
            .run_ended(
                &id,
                generation,
                Outcome::Failed(error_info("internal", format!("internal error: {what}"))),
                Vec::new(),
                Durability::PowerSafe,
            )
            .await;
    }
    let _ = done.send(true);
}

async fn run(inner: Arc<Inner>, spec: RunSpec, mut stop: watch::Receiver<Option<StopReason>>) {
    let id = spec.record.id.clone();
    let generation = spec.generation;
    let mut d = Driver::new(inner.clone(), spec);
    let result = {
        let fut = d.download();
        tokio::pin!(fut);
        tokio::select! {
            biased;
            reason = stopped(&mut stop) => Err(Outcome::Stopped(reason)),
            r = &mut fut => r,
        }
    };
    d.halt().await;
    let outcome = match result {
        Ok(()) => d.finish().await,
        Err(o) => {
            if o != Outcome::Stopped(StopReason::Remove) {
                d.final_checkpoint().await;
            }
            d.publish();
            // The latest request wins (a resume while a pause was flushing).
            match (&o, *stop.borrow()) {
                (Outcome::Stopped(_), Some(latest)) => Outcome::Stopped(latest),
                _ => o,
            }
        }
    };
    d.save_profile().await;
    inner
        .run_ended(&id, generation, outcome, d.segment_infos(), d.durability)
        .await;
}

async fn stopped(rx: &mut watch::Receiver<Option<StopReason>>) -> StopReason {
    loop {
        if let Some(r) = *rx.borrow_and_update() {
            return r;
        }
        if rx.changed().await.is_err() {
            return StopReason::Shutdown;
        }
    }
}

struct Driver {
    inner: Arc<Inner>,
    t: Tuning,
    id: DownloadId,
    generation: u64,
    live: Arc<Live>,
    url: String,
    request: Option<RequestContext>,
    directory: PathBuf,
    filename: String,
    name_fixed: bool,
    connections: Option<u16>,
    /// Recognised as already finished (a crash after `Storage::finish`):
    /// the final name and size.
    adopted: Option<(String, u64)>,
    /// `Settings::durability` when the run began: `ProcessSafe` hands
    /// bytes to the operating system (`Storage::write_out`,
    /// `finish_unsynced`) instead of forcing them to disk.
    durability: Durability,
    /// The journal had an entry for this download when the run began.
    journalled: bool,
    /// What the journal claims (the last commit that succeeded).
    durable: RangeSet,
    /// What the checkpoint in flight will claim.
    ckpt_done: Option<RangeSet>,
    /// Bytes written when the last flush started.
    flushed_upto: u64,
    solo: Option<Solo>,
    /// The first connection's rate alone (past its first burst), bytes/s.
    solo_rate: Option<f64>,
    category_auto: bool,
    checksum: Option<Checksum>,
    source: Option<Arc<dyn RangeSource>>,
    meta: RemoteMeta,
    expect: Validators,
    storage: Option<Arc<dyn Storage>>,
    part: PathBuf,
    size: Option<u64>,
    ranged: bool,
    /// The journal may be committed (a ranged plan is running).
    committable: bool,
    done: RangeSet,
    segs: Vec<Arc<Segment>>,
    workers: HashMap<WorkerId, Wk>,
    tasks: JoinSet<(WorkerId, Exit)>,
    next_wid: WorkerId,
    tuner: Option<Tuner>,
    host: Option<String>,
    profile: Option<HostProfile>,
    rng: fastrand::Rng,
    consecutive: u32,
    open_gate: Instant,
    open_failures: VecDeque<Instant>,
    replacements: u32,
    restarts: u32,
    ttfbs: VecDeque<f64>,
    epoch: Instant,
    recv_total: u64,
    speed: f64,
    per_conn_bps: Option<f64>,
    ckpt: Option<BoxFuture<'static, io::Result<()>>>,
    last_commit: Instant,
    committed: u64,
    /// How long the last checkpoint (flush and commit) took.
    ckpt_cost: Duration,
    ckpt_started: Instant,
    /// This run committed to the journal.
    committed_any: bool,
    last_record: Instant,
    last_tick: Instant,
    last_segments: Instant,
}

impl Driver {
    fn new(inner: Arc<Inner>, spec: RunSpec) -> Self {
        let RunSpec {
            record,
            generation,
            live,
            name_locked,
            connections,
        } = spec;
        let t = inner.config.tuning.clone();
        let rng = match inner.config.rng_seed {
            Some(s) => fastrand::Rng::with_seed(s ^ generation.rotate_left(17)),
            None => fastrand::Rng::new(),
        };
        let now = Instant::now();
        let category_auto =
            record.category.as_deref() == Some(crate::category::category_for(&record.filename));
        Self {
            t,
            id: record.id.clone(),
            generation,
            live,
            url: record.url.clone(),
            request: record.request.clone(),
            directory: PathBuf::from(&record.directory),
            filename: record.filename.clone(),
            name_fixed: name_locked,
            connections,
            adopted: None,
            durability: inner.durability(),
            journalled: false,
            durable: RangeSet::new(),
            ckpt_done: None,
            flushed_upto: 0,
            solo: None,
            solo_rate: None,
            category_auto,
            checksum: record.checksum.clone(),
            source: None,
            meta: RemoteMeta::default(),
            expect: Validators::default(),
            storage: None,
            part: PathBuf::new(),
            size: None,
            ranged: false,
            committable: false,
            done: RangeSet::new(),
            segs: Vec::new(),
            workers: HashMap::new(),
            tasks: JoinSet::new(),
            next_wid: 1,
            tuner: None,
            host: None,
            profile: None,
            rng,
            consecutive: 0,
            open_gate: now,
            open_failures: VecDeque::new(),
            replacements: 0,
            restarts: 0,
            ttfbs: VecDeque::new(),
            epoch: now,
            recv_total: 0,
            speed: 0.0,
            per_conn_bps: None,
            ckpt: None,
            last_commit: now,
            committed: 0,
            ckpt_cost: Duration::ZERO,
            ckpt_started: now,
            committed_any: false,
            last_record: now,
            last_tick: now,
            last_segments: now,
            inner,
        }
    }

    // ------------------------------------------------------------ phases

    async fn state(&mut self, s: DownloadState) {
        self.inner
            .set_state(&self.id, self.generation, s, None)
            .await;
    }

    fn source(&self) -> Arc<dyn RangeSource> {
        self.source.clone().expect("source is set before transfers")
    }

    async fn download(&mut self) -> Result<(), Outcome> {
        match self.recover_finish().await? {
            Recovery::Adopted => return Ok(()),
            // Every byte is on disk already: verify (if asked) and finish.
            Recovery::FinishAgain => return self.verify().await,
            Recovery::None => {}
        }
        self.state(DownloadState::Probing).await;
        let source = self
            .inner
            .deps
            .sources
            .source(&self.url, self.request.as_ref())
            .map_err(|e| Outcome::Failed(source_info(&e)))?;
        self.source = Some(source);
        let mut force_stream = false;
        // A fresh download probes and starts in one request; a resume
        // probes first, to check the validators before taking any byte.
        let (mut meta, mut first) = if self.journalled {
            (self.probe().await?, None)
        } else {
            let (meta, first) = self.start().await?;
            (meta, Some(first))
        };
        loop {
            self.adopt(&meta).await?;
            let plan = match self.prepare(meta.clone(), force_stream).await {
                Ok(p) => p,
                Err(Abort::Remote) => {
                    first = None;
                    meta = self.restart_after_change().await?;
                    continue;
                }
                Err(Abort::NoRanges) => {
                    first = None;
                    force_stream = true;
                    continue;
                }
                Err(Abort::Out(o)) => return Err(o),
            };
            self.state(DownloadState::Downloading).await;
            let r = match plan {
                Plan::Ranged(done) => self.transfer_ranged(done, first.take()).await,
                Plan::Stream => self.transfer_stream(first.take()).await,
            };
            match r {
                Ok(()) => break,
                Err(Abort::Remote) => meta = self.restart_after_change().await?,
                Err(Abort::NoRanges) => {
                    self.halt().await;
                    force_stream = true;
                }
                Err(Abort::Out(o)) => return Err(o),
            }
        }
        self.verify().await
    }

    /// Probe with retries. `LinkExpired` and `Fatal` end the run.
    async fn probe(&mut self) -> Result<RemoteMeta, Outcome> {
        loop {
            match self.source().probe().await {
                Ok(m) => return Ok(m),
                Err(e) => self.probe_failed(e).await?,
            }
        }
    }

    /// `RangeSource::start` with retries, like `probe`: the remote file's
    /// description and its body from offset 0 in one request.
    async fn start(&mut self) -> Result<(RemoteMeta, First), Outcome> {
        loop {
            let called = Instant::now();
            match self.source().start().await {
                Ok((meta, stream)) => return Ok((meta, (stream, called.elapsed()))),
                Err(e) => self.probe_failed(e).await?,
            }
        }
    }

    /// A failed probe or start: end the run, or wait out the backoff.
    async fn probe_failed(&mut self, e: SourceError) -> Result<(), Outcome> {
        match &e {
            SourceError::LinkExpired(_) => return Err(Outcome::NeedsRefresh(source_info(&e))),
            SourceError::Fatal(_) => return Err(Outcome::Failed(source_info(&e))),
            SourceError::Throttled { retry_after, .. } => {
                let ra = *retry_after;
                self.note_failure(&e, ra).map_err(abort_outcome)?;
            }
            _ => self.note_failure(&e, None).map_err(abort_outcome)?,
        }
        tokio::time::sleep_until(self.open_gate).await;
        Ok(())
    }

    /// Finishing is crash-consistent through the journal: before every
    /// rename `finish` commits a checkpoint that claims the whole file at
    /// its final length and names the file it is about to create (an
    /// *intent*), and the journal entry is removed only after the record
    /// says completed. On start, a journal that claims the whole file means
    /// the download had finished downloading:
    ///
    /// - the part file is still there at that length → finish again, no
    ///   network needed (verifying the checksum first, if one was asked);
    /// - the part file is gone and the intent's file exists at exactly that
    ///   length (and passes the checksum, if one was asked) → it was
    ///   renamed just before the process died: completed;
    /// - anything else → download normally.
    ///
    /// This covers every mode: ranged, stream (no ranges or unknown size;
    /// the length is what was written) and empty files.
    async fn recover_finish(&mut self) -> Result<Recovery, Outcome> {
        let Some(cp) = self
            .inner
            .deps
            .journal
            .load(&self.id)
            .await
            .map_err(io_failed)?
        else {
            return Ok(Recovery::None);
        };
        self.journalled = true;
        self.durable = cp.done.clone();
        let Some(size) = cp.meta.size else {
            return Ok(Recovery::None);
        };
        let provider = self.inner.deps.storage.clone();
        let Some(part) = names::part_path(&self.directory, &self.filename) else {
            return Ok(Recovery::None);
        };
        let part_len = provider.file_len(&part).await.map_err(io_failed)?;
        let all = cp.done.len() == size
            && cp.done.contains(ByteRange {
                start: 0,
                end: size,
            });
        match part_len {
            Some(len) if all && len == size => {
                tracing::info!(id = %self.id, "downloaded before a restart; finishing");
                self.part = part;
                self.size = Some(size);
                self.meta = without_intent(cp.meta);
                self.ranged = false;
                self.open_storage().await?;
                return Ok(Recovery::FinishAgain);
            }
            Some(_) => return Ok(Recovery::None),
            None => {}
        }
        // The part file is gone. The intent's file at exactly the intent's
        // length is ours: `finish` renames only after the bytes are
        // flushed (whatever the intent itself claims).
        let Some(Finishing { name, len }) = intent(&cp) else {
            return Ok(Recovery::None);
        };
        if len != size {
            return Ok(Recovery::None);
        }
        let Some(path) = names::inside(&self.directory, &name) else {
            return Ok(Recovery::None);
        };
        if provider.file_len(&path).await.map_err(io_failed)? != Some(size) {
            return Ok(Recovery::None);
        }
        if let Some(ck) = self.checksum.clone() {
            let Ok((algo, want)) = checksum::parse(&ck) else {
                return Ok(Recovery::None);
            };
            let Ok(file) = provider.open(&path).await else {
                return Ok(Recovery::None);
            };
            let got = checksum::digest(&*file, size, algo, self.t.verify_chunk).await;
            if !matches!(got, Ok(got) if got == want) {
                return Ok(Recovery::None);
            }
        }
        tracing::info!(id = %self.id, name, "finished just before a restart; adopting it");
        self.size = Some(size);
        self.adopted = Some((name, size));
        Ok(Recovery::Adopted)
    }

    /// Take in what a probe learned: file name (only before a part file
    /// exists), size, host profile.
    async fn adopt(&mut self, meta: &RemoteMeta) -> Result<(), Outcome> {
        self.meta = meta.clone();
        self.size = meta.size;
        // An entry exists if there was one when the run began or the run
        // committed one since.
        let journalled = self.journalled || self.committed_any;
        if !self.name_fixed && !journalled {
            let url = meta.final_url.as_deref().unwrap_or(&self.url);
            let mut name = names::resolve(
                None,
                meta.filename.as_deref(),
                url,
                meta.content_type.as_deref(),
            );
            if name != self.filename {
                // Checked and stored in one critical section (two probes
                // naming the same file must not share a part file).
                if let Some(reserved) =
                    self.inner
                        .reserve_name(&self.id, self.generation, &name, self.category_auto)
                {
                    name = reserved;
                    self.filename = name;
                }
            }
        }
        self.name_fixed = true;
        self.part = names::part_path(&self.directory, &self.filename).ok_or_else(|| {
            Outcome::Failed(error_info("invalid_name", "file name leaves the directory"))
        })?;
        let filename = self.filename.clone();
        let category_auto = self.category_auto;
        let size = meta.size;
        let resumable = meta.accepts_ranges && meta.size.is_some();
        self.inner
            .update(&self.id, self.generation, |r| {
                if r.filename != filename && category_auto {
                    r.category = Some(crate::category::category_for(&filename).to_string());
                }
                r.filename = filename;
                r.total_bytes = size;
                r.resumable = resumable;
            })
            .await;
        // Clients learned the add-time name; tell them the real one.
        self.inner.announce(&self.id);
        self.live
            .total
            .store(size.unwrap_or(u64::MAX), Ordering::Relaxed);
        if self.tuner.is_none() {
            self.host = host_key(meta.final_url.as_deref().unwrap_or(&self.url));
            if let Some(h) = &self.host {
                self.profile = self.inner.deps.state.host_profile(h).await.ok().flatten();
            }
            self.tuner = Some(Tuner::new(
                &self.t,
                self.inner.max_connections(),
                self.profile.as_ref(),
                self.connections,
            ));
        }
        Ok(())
    }

    async fn open_storage(&mut self) -> Result<Arc<dyn Storage>, Outcome> {
        if let Some(s) = &self.storage {
            return Ok(s.clone());
        }
        let s = self
            .inner
            .deps
            .storage
            .open(&self.part)
            .await
            .map_err(io_failed)?;
        self.storage = Some(s.clone());
        Ok(s)
    }

    /// Decide how to download: resume a ranged download from the journal
    /// (after checking validators and segment tails), or stream from zero.
    async fn prepare(&mut self, meta: RemoteMeta, force_stream: bool) -> Result<Plan, Abort> {
        self.ranged = !force_stream && meta.accepts_ranges && meta.size.is_some();
        // Nothing journalled and no part file yet (a new download): the
        // file is created empty, with nothing to discard.
        let journalled = self.journalled || self.committed_any;
        let pristine = !journalled
            && self.storage.is_none()
            && self
                .inner
                .deps
                .storage
                .file_len(&self.part)
                .await
                .map_err(io_failed)?
                .is_none();
        self.open_storage().await?;
        if !self.ranged {
            // Every attempt starts from zero, but still on this version: a
            // changed file (size included) must not be cut or padded.
            self.expect = meta.validators();
            if !pristine {
                self.discard().await?;
            }
            return Ok(Plan::Stream);
        }
        let size = meta.size.unwrap_or(0);
        let cp = if journalled {
            self.inner
                .deps
                .journal
                .load(&self.id)
                .await
                .map_err(io_failed)?
        } else {
            None
        };
        let mut expect = meta.validators();
        let mut done = RangeSet::new();
        if let Some(cp) = cp {
            if cp.meta.validators().conflicts_with(&expect) {
                return Err(Abort::Remote);
            }
            // Pin every request to the version we already hold.
            expect.etag = expect.etag.or(cp.meta.etag);
            expect.last_modified = expect.last_modified.or(cp.meta.last_modified);
            // Ranges are only meaningful for a checkpoint of this size (a
            // stream attempt journals no size and no ranges).
            if cp.meta.size == Some(size) {
                for r in cp.done.ranges() {
                    if let Some(c) = ByteRange::new(r.start.min(size), r.end.min(size)) {
                        done.insert(c);
                    }
                }
            }
        }
        self.meta.etag = expect.etag.clone();
        self.meta.last_modified = expect.last_modified.clone();
        self.expect = expect;
        if !done.is_empty() {
            let verified = self.tail_verify(done.clone()).await?;
            if verified != done {
                // Stop claiming what failed verification before anything
                // overwrites it (shrinking needs no flush).
                let cp = Checkpoint {
                    meta: self.meta.clone(),
                    done: verified.clone(),
                    finishing: None,
                };
                self.committed_any = true;
                self.inner
                    .deps
                    .journal
                    .commit(&self.id, &cp)
                    .await
                    .map_err(io_failed)?;
                self.durable = verified.clone();
            }
            done = verified;
        }
        if done.is_empty() && !pristine {
            // Nothing journalled: whatever a `<name>.vayu-part` already holds
            // (a removed download that kept its files, a crash before the
            // first commit, an older version) is not ours. Start from an
            // empty file, so `finish` can never move stale bytes along.
            self.discard().await?;
        }
        if size > 0 {
            let storage = self.open_storage().await?;
            storage.preallocate(size).await.map_err(io_failed)?;
        }
        Ok(Plan::Ranged(done))
    }

    /// Forget every byte: commit an empty checkpoint first (the journal
    /// never claims more than the file holds), then recreate the file. A
    /// download the journal has no entry for (fresh, nothing committed yet)
    /// skips the commit: nothing claims anything.
    async fn discard(&mut self) -> Result<(), Outcome> {
        self.committable = false;
        // A flush in flight lands before anything newer (its error no
        // longer matters: every byte is about to go).
        if let Some(c) = self.ckpt.take() {
            let r = c.await;
            self.ckpt_landed(r.is_ok());
        }
        self.flushed_upto = 0;
        if self.journalled || self.committed_any {
            let cp = Checkpoint {
                meta: self.meta.clone(),
                done: RangeSet::new(),
                finishing: None,
            };
            self.committed_any = true;
            self.inner
                .deps
                .journal
                .commit(&self.id, &cp)
                .await
                .map_err(io_failed)?;
            self.durable = RangeSet::new();
        }
        self.storage = None;
        self.inner
            .deps
            .storage
            .remove(&self.part)
            .await
            .map_err(io_failed)?;
        self.open_storage().await?;
        self.done = RangeSet::new();
        Ok(())
    }

    async fn restart_after_change(&mut self) -> Result<RemoteMeta, Outcome> {
        self.halt().await;
        if self.restarts >= self.t.max_remote_restarts {
            return Err(Outcome::Failed(error_info(
                "remote_changed",
                "the file changed on the server again while downloading; \
                 refusing to mix versions",
            )));
        }
        self.restarts += 1;
        tracing::info!(id = %self.id, "remote file changed; restarting from zero");
        self.state(DownloadState::Probing).await;
        let meta = self.probe().await?;
        self.meta = meta.clone();
        self.discard().await?;
        Ok(meta)
    }

    // ------------------------------------------------------ verification

    /// Re-fetch the last `tail_verify_len` bytes of every journalled range
    /// and compare them with storage. A mismatch drops that window (and
    /// walks back up to `tail_walkback` windows; past that, the whole
    /// range). Without an ETag or Last-Modified a mismatch cannot tell
    /// local damage from a new remote version, so it restarts from zero.
    async fn tail_verify(&mut self, done: RangeSet) -> Result<RangeSet, Abort> {
        let tail = self.t.tail_verify_len.max(1);
        let windows: Vec<(ByteRange, ByteRange)> = done
            .ranges()
            .iter()
            .map(|r| {
                let w = tail.min(r.len());
                (
                    *r,
                    ByteRange {
                        start: r.end - w,
                        end: r.end,
                    },
                )
            })
            .collect();
        // First pass: every window at once, no retries.
        let source = self.source();
        let storage = self.open_storage().await?;
        let expect = self.expect.clone();
        let parallel = usize::from(self.tuner.as_ref().map_or(4, Tuner::target)).max(1);
        let first: Vec<(ByteRange, ByteRange, Result<Tail, SourceError>)> = stream::iter(windows)
            .map(|(r, win)| {
                let (source, storage, expect) = (source.clone(), storage.clone(), expect.clone());
                async move { (r, win, compare(&*source, &*storage, &expect, win).await) }
            })
            .buffered(parallel)
            .collect()
            .await;
        let no_validators = self.expect.etag.is_none() && self.expect.last_modified.is_none();
        let mut out = done.clone();
        for (range, win, res) in first {
            let mut verdict = match res {
                Ok(v) => v,
                Err(_) => self.compare_retrying(win).await?,
            };
            let mut win = win;
            let mut walked = 0;
            loop {
                match verdict {
                    Tail::Match => break,
                    Tail::LocalShort => {
                        // The local copy is gone or truncated: nothing of
                        // this range can be trusted, and the server is not
                        // to blame.
                        out.remove(ByteRange {
                            start: range.start,
                            end: win.end,
                        });
                        break;
                    }
                    Tail::Mismatch if no_validators => {
                        tracing::info!(id = %self.id, "tail mismatch without validators; restarting");
                        return Err(Abort::Remote);
                    }
                    Tail::Mismatch => {}
                }
                out.remove(win);
                walked += 1;
                if win.start <= range.start {
                    break;
                }
                if walked > self.t.tail_walkback {
                    out.remove(ByteRange {
                        start: range.start,
                        end: win.start,
                    });
                    break;
                }
                let w = tail.min(win.start - range.start);
                win = ByteRange {
                    start: win.start - w,
                    end: win.start,
                };
                verdict = self.compare_retrying(win).await?;
            }
        }
        Ok(out)
    }

    async fn compare_retrying(&mut self, win: ByteRange) -> Result<Tail, Abort> {
        loop {
            let storage = self.open_storage().await?;
            match compare(&*self.source(), &*storage, &self.expect, win).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    self.on_source_error(e, false)?;
                    tokio::time::sleep_until(self.open_gate).await;
                }
            }
        }
    }

    async fn verify(&mut self) -> Result<(), Outcome> {
        let Some(ck) = self.checksum.clone() else {
            return Ok(());
        };
        let (algo, want) =
            checksum::parse(&ck).map_err(|m| Outcome::Failed(error_info("invalid_checksum", m)))?;
        self.state(DownloadState::Verifying).await;
        let storage = self.open_storage().await?;
        let len = self.size.unwrap_or(0);
        let got = checksum::digest(&*storage, len, algo, self.t.verify_chunk)
            .await
            .map_err(io_failed)?;
        if got != want {
            // Start clean if the user retries.
            let _ = self.discard().await;
            return Err(Outcome::Failed(error_info(
                "checksum_mismatch",
                format!(
                    "{} mismatch: expected {}, got {}",
                    ck.algorithm,
                    hex::encode(&want),
                    hex::encode(&got)
                ),
            )));
        }
        Ok(())
    }

    // ------------------------------------------------------------ finish

    /// Move the part file to its final name, numbering on collisions. Not
    /// cancellable.
    async fn finish(&mut self) -> Outcome {
        if let Some((filename, total)) = self.adopted.take() {
            return Outcome::Completed { filename, total };
        }
        let storage = match self.open_storage().await {
            Ok(s) => s,
            Err(o) => return o,
        };
        let len = self.size.unwrap_or(0);
        let mut flushed = false;
        let mut chosen = None;
        for n in 0..10_000u32 {
            let candidate = if n == 0 {
                self.filename.clone()
            } else {
                names::numbered(&self.filename, n)
            };
            let Some(path) = names::inside(&self.directory, &candidate) else {
                return Outcome::Failed(error_info(
                    "invalid_name",
                    "file name leaves the directory",
                ));
            };
            // Intent first, durably: if the process dies after the rename,
            // the next start knows which file is ours. The first one is
            // committed while the data is flushed (the two syncs overlap),
            // so it claims only what the journal already did; later ones
            // follow that flush and claim every byte. The rename waits for
            // both.
            let intent = Checkpoint {
                meta: RemoteMeta {
                    size: Some(len),
                    ..without_intent(self.meta.clone())
                },
                done: if flushed {
                    RangeSet::from_ranges(ByteRange::new(0, len))
                } else {
                    self.durable.clone()
                },
                finishing: Some(Finishing {
                    name: candidate.clone(),
                    len,
                }),
            };
            let commit = self.inner.deps.journal.commit(&self.id, &intent);
            let committed = if flushed {
                commit.await
            } else {
                let (f, c) = futures::join!(persist(&*storage, self.durability), commit);
                if let Err(e) = f {
                    return io_failed(e);
                }
                flushed = true;
                c
            };
            if let Err(e) = committed {
                return io_failed(e);
            }
            let moved = match self.durability {
                Durability::PowerSafe => storage.finish(&path).await,
                Durability::ProcessSafe => storage.finish_unsynced(&path).await,
            };
            match moved {
                Ok(()) => {
                    chosen = Some(candidate);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return io_failed(e),
            }
        }
        let Some(filename) = chosen else {
            return Outcome::Failed(error_info("io", "no free file name"));
        };
        // The journal entry goes only after the record says completed
        // (`Inner::run_ended`).
        Outcome::Completed {
            filename,
            total: len,
        }
    }

    /// Stop every connection and wait until none can write any more.
    async fn halt(&mut self) {
        for w in self.workers.values() {
            w.cancel.cancel();
        }
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
        self.workers.clear();
        // Let a commit in flight land before anything newer (an empty
        // checkpoint on restart, the final one on stop) is written.
        if let Some(c) = self.ckpt.take() {
            let r = c.await;
            self.ckpt_landed(r.is_ok());
            if let Err(e) = r {
                tracing::warn!(id = %self.id, "checkpoint failed: {e}");
            }
        }
    }

    /// Flush and commit what is written, when stopping without finishing.
    async fn final_checkpoint(&mut self) {
        if !(self.committable && self.ranged) {
            return;
        }
        if let Err(e) = self.checkpoint_now().await {
            tracing::warn!(id = %self.id, "final checkpoint failed: {e}");
        }
    }

    async fn save_profile(&mut self) {
        let (Some(host), Some(tuner)) = (&self.host, &self.tuner) else {
            return;
        };
        if self.recv_total == 0 {
            return;
        }
        let prev = self.profile.clone().unwrap_or_default();
        let learned = self.t.fixed_connections.is_none()
            && self.connections.is_none()
            && (tuner.settled() || tuner.max_ok().is_some());
        let profile = HostProfile {
            host: host.clone(),
            best_connections: if learned {
                Some(tuner.best())
            } else {
                prev.best_connections
            },
            max_ok_connections: tuner.max_ok().or(prev.max_ok_connections),
            accepts_ranges: Some(self.meta.accepts_ranges),
            protocol: self.meta.protocol.or(prev.protocol),
            per_connection_bps: self
                .per_conn_bps
                .map(|v| v as u64)
                .or(prev.per_connection_bps),
            updated_at: unix_millis(),
        };
        if let Err(e) = self.inner.deps.state.save_host_profile(&profile).await {
            tracing::warn!("saving host profile failed: {e}");
        }
    }

    pub(crate) fn segment_infos(&self) -> Vec<SegmentInfo> {
        let label = self
            .source
            .as_ref()
            .map_or_else(String::new, |s| s.label().to_string());
        let mut out: Vec<SegmentInfo> = self
            .done
            .ranges()
            .iter()
            .map(|r| SegmentInfo {
                start: r.start,
                end: r.end,
                downloaded_bytes: r.len(),
                speed_bps: 0,
                source: label.clone(),
            })
            .collect();
        for s in &self.segs {
            let snap = s.snapshot();
            let speed = snap
                .owner
                .and_then(|o| self.workers.get(&o))
                .and_then(|w| w.ewma)
                .unwrap_or(0.0);
            out.push(SegmentInfo {
                start: snap.start,
                end: if snap.end == u64::MAX {
                    snap.written
                } else {
                    snap.end
                },
                downloaded_bytes: snap.written - snap.start,
                speed_bps: speed as u64,
                source: label.clone(),
            });
        }
        out.sort_by_key(|s| s.start);
        out
    }

    // ------------------------------------------------------- transfers

    /// `first`: the response `start` began (a fresh download). Its
    /// connection owns the whole file and runs alone until splitting pays
    /// (`solo_check`); otherwise the first plan is one range per connection.
    async fn transfer_ranged(&mut self, done: RangeSet, first: Option<First>) -> Result<(), Abort> {
        let size = self.size.unwrap_or(0);
        self.done = done;
        self.segs = self
            .done
            .gaps(size)
            .into_iter()
            .map(|g| Segment::new(g.start, g.end))
            .collect();
        let first = first.filter(|_| self.done.is_empty() && self.segs.len() == 1);
        if let Some((stream, ttfb)) = first {
            let seg = self.segs[0].clone();
            if self.spawn_first(seg.clone(), stream, ttfb).is_some() && self.target() > 1 {
                self.solo = Some(Solo {
                    seg,
                    ttfb: ttfb.as_secs_f64(),
                    next: Instant::now() + self.t.solo_check,
                    mark: None,
                });
            }
        }
        if self.solo.is_none() {
            self.plan();
        }
        self.committable = true;
        let now = Instant::now();
        self.last_commit = now;
        self.committed = self.done.len();
        let mut tick = tokio::time::interval_at(now + self.t.tick, self.t.tick);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut wb = tokio::time::interval_at(now + self.t.writeback_check, self.t.writeback_check);
        wb.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            self.collect();
            if self.segs.is_empty() {
                break;
            }
            self.solo_check(Instant::now());
            self.fill();
            let ckpt = &mut self.ckpt;
            let solo_next = self.solo.as_ref().map(|s| s.next);
            tokio::select! {
                Some(j) = self.tasks.join_next(), if !self.tasks.is_empty() => self.on_exit(j)?,
                _ = tick.tick() => self.on_tick().await?,
                r = async { match ckpt.as_mut() { Some(f) => f.await, None => std::future::pending().await } } => {
                    self.ckpt = None;
                    self.ckpt_cost = self.ckpt_started.elapsed();
                    self.ckpt_landed(r.is_ok());
                    r.map_err(io_failed)?;
                }
                _ = tokio::time::sleep_until(solo_next.unwrap_or_else(Instant::now)), if solo_next.is_some() => {}
                _ = wb.tick() => self.writeback(),
            }
        }
        self.solo = None;
        if let Some(c) = self.ckpt.take() {
            let r = c.await;
            self.ckpt_landed(r.is_ok());
            r.map_err(io_failed)?;
        }
        self.halt().await;
        // Finishing flushes and commits a checkpoint claiming every byte
        // right away; only a checksum to verify first (a long read) is
        // worth a checkpoint of its own.
        if self.checksum.is_some() {
            self.checkpoint_now().await.map_err(io_failed)?;
        }
        self.publish();
        Ok(())
    }

    /// Ranges unsupported or size unknown: one connection from the start.
    /// A failure discards everything and starts again (after backoff).
    async fn transfer_stream(&mut self, mut first: Option<First>) -> Result<(), Abort> {
        self.committable = false;
        loop {
            // Always read to the end of the response: a body of another
            // length is another version, never cut or padded to fit.
            let seg = Segment::open_ended(0);
            self.segs = vec![seg.clone()];
            match first.take() {
                Some((stream, ttfb)) => {
                    self.spawn_first(seg.clone(), stream, ttfb);
                }
                None => self.spawn(Work::Own(seg.clone())),
            }
            let now = Instant::now();
            let mut tick = tokio::time::interval_at(now + self.t.tick, self.t.tick);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut wb =
                tokio::time::interval_at(now + self.t.writeback_check, self.t.writeback_check);
            wb.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let joined = loop {
                let ckpt = &mut self.ckpt;
                tokio::select! {
                    Some(j) = self.tasks.join_next() => break j,
                    _ = tick.tick() => {
                        self.on_tick().await?;
                        self.stream_idle_check()?;
                    }
                    _ = wb.tick() => self.writeback(),
                    r = async { match ckpt.as_mut() { Some(f) => f.await, None => std::future::pending().await } } => {
                        self.ckpt = None;
                        self.ckpt_landed(r.is_ok());
                        r.map_err(io_failed)?;
                    }
                }
            };
            let (wid, exit) = match joined {
                Ok(v) => v,
                Err(e) => return Err(panicked(e)),
            };
            if let Some(w) = self.workers.remove(&wid) {
                self.recv_total += w.counters.bytes.load(Ordering::Relaxed) - w.last_bytes;
            }
            match exit {
                Exit::Finished => {
                    let snap = seg.snapshot();
                    if self.size.is_some_and(|n| n != snap.end) {
                        tracing::info!(id = %self.id, "response length differs from the probe");
                        return Err(Abort::Remote);
                    }
                    self.size = Some(snap.end);
                    self.live.total.store(snap.end, Ordering::Relaxed);
                    let end = snap.end;
                    self.inner
                        .update(&self.id, self.generation, |r| r.total_bytes = Some(end))
                        .await;
                    self.done = RangeSet::from_ranges(ByteRange::new(0, end));
                    self.segs.clear();
                    self.publish();
                    return Ok(());
                }
                Exit::Failed { err, streamed } => {
                    if matches!(err, SourceError::RangesUnsupported) {
                        return Err(Abort::Out(Outcome::Failed(source_info(&err))));
                    }
                    self.on_source_error(err, streamed)?;
                    tokio::time::sleep_until(self.open_gate).await;
                    self.discard().await?;
                }
                Exit::Storage(e) => return Err(io_failed(e).into()),
                Exit::Lost | Exit::Cancelled => {
                    tokio::time::sleep_until(self.open_gate).await;
                    self.discard().await?;
                }
            }
        }
    }

    /// Stream mode cannot hedge (no ranges), so a response silent for
    /// `stream_idle_timeout` (before or after its first byte) is cancelled
    /// and the download starts again from zero, as a failure round.
    fn stream_idle_check(&mut self) -> Result<(), Abort> {
        let now = Instant::now();
        let now_us = now_us_since(self.epoch, now);
        let limit = self.t.stream_idle_timeout;
        let mut stalled = false;
        for w in self.workers.values_mut() {
            if w.cancelling {
                continue;
            }
            let last = w.counters.last_us.load(Ordering::Relaxed);
            let silent = if last != 0 {
                Duration::from_micros(now_us.saturating_sub(last))
            } else {
                now.duration_since(w.started)
            };
            if silent >= limit {
                w.cancelling = true;
                w.cancel.cancel();
                stalled = true;
            }
        }
        if stalled {
            tracing::debug!(id = %self.id, "stream stalled; starting again");
            self.note_failure(&SourceError::Transient("stream stalled".into()), None)?;
        }
        Ok(())
    }

    // ------------------------------------------------------- scheduling

    fn target(&self) -> u16 {
        self.tuner.as_ref().map_or(1, Tuner::target)
    }

    /// Connections that count against the target: everything not being
    /// cancelled (owners and pending hedges).
    fn active(&self) -> usize {
        self.workers.values().filter(|w| !w.cancelling).count()
    }

    /// Connections still waiting for their response headers.
    fn unconfirmed(&self) -> usize {
        self.workers
            .values()
            .filter(|w| !w.cancelling && w.counters.opened_us.load(Ordering::Relaxed) == 0)
            .count()
    }

    /// Owners whose request the server accepted (headers checked).
    fn working_owners(&self) -> u16 {
        self.workers
            .iter()
            .filter(|(wid, w)| {
                !w.cancelling
                    && w.counters.opened_us.load(Ordering::Relaxed) != 0
                    && w.seg.snapshot().owner == Some(**wid)
            })
            .count() as u16
    }

    /// Owners that have delivered their first byte.
    fn streaming_owners(&self) -> u16 {
        self.workers
            .iter()
            .filter(|(wid, w)| {
                !w.cancelling && w.first_byte() && w.seg.snapshot().owner == Some(**wid)
            })
            .count() as u16
    }

    /// Typical per-connection speed: the median over live connections, or,
    /// once fewer than two remain (the endgame), the last median seen while
    /// more were running, so a lone slow connection is still recognised.
    fn median_speed(&self) -> Option<f64> {
        let mut v: Vec<f64> = self
            .workers
            .values()
            .filter(|w| !w.cancelling)
            .filter_map(|w| w.ewma)
            .filter(|s| *s > 0.0)
            .collect();
        if v.len() >= 2 {
            return median(&mut v);
        }
        self.per_conn_bps.or_else(|| v.first().copied())
    }

    fn median_ttfb(&self) -> Option<f64> {
        let mut v: Vec<f64> = self.ttfbs.iter().copied().collect();
        median(&mut v)
    }

    /// Expected time between chunks on a healthy connection, seconds.
    fn expected_gap(&self) -> Option<f64> {
        let speed = self.median_speed()?;
        let (bytes, chunks) = self.workers.values().fold((0u64, 0u64), |(b, c), w| {
            (
                b + w.counters.bytes.load(Ordering::Relaxed),
                c + w.counters.chunks.load(Ordering::Relaxed),
            )
        });
        (chunks > 0).then(|| (bytes as f64 / chunks as f64) / speed)
    }

    /// Move fully written segments into `done`; cancel connections still
    /// attached to them (a hedge that never delivered).
    fn collect(&mut self) {
        let mut i = 0;
        while i < self.segs.len() {
            let snap = self.segs[i].snapshot();
            if snap.complete() {
                let seg = self.segs.swap_remove(i);
                if let Some(r) = ByteRange::new(snap.start, snap.end) {
                    self.done.insert(r);
                }
                for w in self.workers.values_mut() {
                    if Arc::ptr_eq(&w.seg, &seg) {
                        w.cancel.cancel();
                    }
                }
            } else {
                i += 1;
            }
        }
    }

    /// One range per connection (up to the target, and no more than the
    /// first wave of `initial_connections` opens: later connections split
    /// their work off as they start), never below the minimum: halve the
    /// largest remaining range until there are enough.
    fn plan(&mut self) {
        let target = usize::from(self.target().min(self.t.initial_connections.max(1)));
        while self.segs.len() < target {
            let Some(big) = self
                .segs
                .iter()
                .max_by_key(|s| s.snapshot().remaining())
                .cloned()
            else {
                break;
            };
            match big.split(self.min_split(), 0.5, self.t.split_align) {
                Some(tail) => self.segs.push(tail),
                None => break,
            }
        }
    }

    /// The smallest piece worth a connection: `min_split`, or on a slow
    /// link (once speeds are known) what the median connection delivers in
    /// `max(min_split_time, 4 × TTFB)`, but at least `min_split_floor`.
    fn min_split(&self) -> u64 {
        let hi = self.t.min_split;
        let Some(speed) = self.median_speed().or(self.solo_rate) else {
            return hi;
        };
        let secs =
            (4.0 * self.median_ttfb().unwrap_or(0.0)).max(self.t.min_split_time.as_secs_f64());
        // A multiple of the split alignment, so split points stay aligned.
        let align = self.t.split_align.max(1);
        ((speed * secs) as u64)
            .clamp(self.t.min_split_floor.min(hi), hi)
            .div_ceil(align)
            .saturating_mul(align)
            .min(hi)
    }

    /// While a fresh download's first range has one connection (`Solo`),
    /// decide whether to add the others: once the rest would take it longer
    /// than `solo_ttfb_factor` × a new connection's time to first byte
    /// (what `start` took) and longer than `solo_min_eta`, or once it has
    /// been silent for `max(solo_silence, 4 × that time)`. Then the range is
    /// split for the target count, as the first plan of a resume would be.
    /// A connection that breaks is replaced by one (the rest is small by
    /// this rule); one that a stall hedge overtook is followed.
    fn solo_check(&mut self, now: Instant) {
        let Some(solo) = self.solo.clone() else {
            return;
        };
        let mut mark = solo.mark;
        let owner = solo.seg.snapshot().owner;
        let end = match owner.map(|o| (o, self.workers.get(&o))) {
            // Nobody owns it right now: its connection broke, and `fill`
            // hands the rest to one new connection, again alone (what is
            // left would take one connection less than a new one needs).
            None => SoloEnd::Hold,
            Some((wid, Some(w))) if !w.cancelling => {
                let n = w.seg.snapshot();
                let opened = w.counters.opened_us.load(Ordering::Relaxed);
                let now_us = now_us_since(self.epoch, now);
                let watched = now_us.saturating_sub(opened) as f64 / 1e6;
                let bytes = w.counters.bytes.load(Ordering::Relaxed);
                let last = w.counters.last_us.load(Ordering::Relaxed);
                let silent = now_us.saturating_sub(last) as f64 / 1e6;
                let quiet_limit = (4.0 * solo.ttfb).max(self.t.solo_silence.as_secs_f64());
                if last != 0 && silent >= quiet_limit {
                    // Gone quiet while alone: the others take the rest
                    // (and the stall checks deal with this one).
                    SoloEnd::Other
                } else if opened == 0 || watched < self.t.solo_check.as_secs_f64() || bytes == 0 {
                    SoloEnd::Hold
                } else {
                    match mark {
                        Some((m, at, b)) if m == wid && bytes > b && now > at => {
                            // Past the first burst: from the previous look.
                            let rate = (bytes - b) as f64 / now.duration_since(at).as_secs_f64();
                            let eta = n.remaining() as f64 / rate;
                            let worth = (self.t.solo_ttfb_factor * solo.ttfb)
                                .max(self.t.solo_min_eta.as_secs_f64());
                            if eta > worth {
                                SoloEnd::Eta(rate)
                            } else {
                                SoloEnd::Hold
                            }
                        }
                        // Nothing new since the previous look: keep it, so
                        // the next rate spans a whole chunk.
                        Some((m, _, b)) if m == wid && bytes == b => SoloEnd::Hold,
                        _ => {
                            mark = Some((wid, now, bytes));
                            SoloEnd::Hold
                        }
                    }
                }
            }
            _ => SoloEnd::Other,
        };
        if matches!(end, SoloEnd::Hold) && self.target() > 1 {
            if let Some(s) = self.solo.as_mut() {
                s.mark = mark;
                if now >= s.next {
                    s.next = now + self.t.solo_check;
                }
            }
            return;
        }
        if let SoloEnd::Eta(rate) = end {
            self.solo_rate = Some(rate);
            // Less than TCP's initial window per round trip, past the first
            // burst: the server paces this connection (any path carries
            // more). Every connection will be limited alike, so start many
            // at once.
            if rate * solo.ttfb.max(1e-3) < self.t.solo_window as f64
                && let Some(t) = self.tuner.as_mut()
            {
                tracing::debug!(id = %self.id, rate, ttfb = solo.ttfb, "first connection limited on its own");
                t.limited_per_connection(self.t.limited_connections, rate);
            }
        }
        tracing::debug!(id = %self.id, "first range split for the others");
        self.solo = None;
        self.plan();
    }

    /// Give every free connection slot work: ownerless ranges first, then
    /// the largest remaining range of the slowest connection (split), then
    /// endgame hedges.
    fn fill(&mut self) {
        let now = Instant::now();
        loop {
            if self.active() >= usize::from(self.target()) {
                break;
            }
            // At most `initial_connections` requests wait for their headers
            // at once: a jump to many connections opens in waves, so a
            // server that caps connections refuses one wave (and sets the
            // ceiling), not all of them.
            if self.unconfirmed() >= usize::from(self.t.initial_connections.max(1)) {
                break;
            }
            let gated = now < self.open_gate;
            if gated && self.replacements == 0 {
                break;
            }
            let Some(work) = self.pick(now, self.solo.is_some()) else {
                break;
            };
            if gated {
                self.replacements -= 1;
            }
            self.spawn(work);
        }
        self.replacements = 0;
    }

    /// `solo`: only ranges nobody owns (the first connection runs alone).
    fn pick(&mut self, now: Instant, solo: bool) -> Option<Work> {
        // 1. Ownerless ranges (a connection failed or was cancelled).
        if let Some(seg) = self
            .segs
            .iter()
            .map(|s| (s, s.snapshot()))
            .filter(|(_, n)| n.owner.is_none() && n.remaining() > 0)
            .max_by_key(|(_, n)| n.remaining())
            .map(|(s, _)| s.clone())
        {
            return Some(Work::Own(seg));
        }
        if solo {
            return None;
        }
        let median_speed = self.median_speed();
        // 2. Work stealing: the range that would finish last.
        let min = self.min_split();
        let mut victim: Option<(Arc<Segment>, f64, f64)> = None;
        for s in &self.segs {
            let n = s.snapshot();
            if n.challenger.is_some() || n.remaining() < 2 * min {
                continue;
            }
            let Some(owner) = n.owner else { continue };
            let v = self
                .workers
                .get(&owner)
                .and_then(|w| w.ewma)
                .or(median_speed)
                .unwrap_or(1.0)
                .max(1.0);
            let score = n.remaining() as f64 / v;
            if victim.as_ref().is_none_or(|(_, best, _)| score > *best) {
                victim = Some((s.clone(), score, v));
            }
        }
        if let Some((seg, _, v)) = victim {
            let u = median_speed.unwrap_or(v).max(1.0);
            if let Some(tail) = seg.split(min, v / (v + u), self.t.split_align) {
                self.segs.push(tail.clone());
                return Some(Work::Own(tail));
            }
        }
        // 3. Endgame: hedge the connection that would finish last, when a
        //    fresh connection would finish much sooner.
        let speed = median_speed?;
        let ttfb = self.median_ttfb().unwrap_or(0.0);
        let gap = self.expected_gap().unwrap_or(0.0);
        let silence_limit = self.t.endgame_silence.as_secs_f64().max(3.0 * gap);
        let mut best: Option<(Arc<Segment>, f64, HedgeKind)> = None;
        for (wid, w) in &self.workers {
            if w.cancelling {
                continue;
            }
            let n = w.seg.snapshot();
            if n.owner != Some(*wid)
                || n.challenger.is_some()
                || n.hedges >= self.t.max_hedges_per_range
                || n.remaining() == 0
            {
                continue;
            }
            let rem = n.remaining() as f64;
            let fresh = ttfb + rem / speed;
            let (owner_eta, kind) = if w.first_byte() {
                let silent = now_us_since(self.epoch, now)
                    .saturating_sub(w.counters.last_us.load(Ordering::Relaxed))
                    as f64
                    / 1e6;
                if silent >= silence_limit {
                    (f64::INFINITY, HedgeKind::Stall)
                } else {
                    (rem / w.ewma.unwrap_or(speed).max(1.0), HedgeKind::Slow)
                }
            } else {
                let waited = now.duration_since(w.started).as_secs_f64();
                if waited > self.first_byte_limit().as_secs_f64() {
                    (f64::INFINITY, HedgeKind::Stall)
                } else {
                    ((ttfb - waited).max(0.0) + rem / speed, HedgeKind::Slow)
                }
            };
            if owner_eta > self.t.hedge_gain * fresh {
                let gain = owner_eta - fresh;
                if best.as_ref().is_none_or(|(_, g, _)| gain > *g) {
                    best = Some((w.seg.clone(), gain, kind));
                }
            }
        }
        best.map(|(seg, _, kind)| Work::Hedge(seg, kind))
    }

    fn first_byte_limit(&self) -> Duration {
        match self.median_ttfb() {
            Some(t) => self
                .t
                .stall_min
                .max(Duration::from_secs_f64(t * self.t.first_byte_factor)),
            None => self.t.first_byte_timeout,
        }
    }

    fn stall_limit(&self) -> Duration {
        match self.expected_gap() {
            Some(g) => self
                .t
                .stall_min
                .max(Duration::from_secs_f64((g * self.t.stall_factor).min(60.0))),
            None => self.t.stall_min,
        }
    }

    /// Start a connection. Stream mode (`self.ranged == false`) requests
    /// the whole file.
    fn spawn(&mut self, work: Work) {
        self.spawn_job(work, None);
    }

    /// Hand the response `start` began to the first connection, as owner
    /// of `seg` (which starts at offset 0). Returns its id.
    fn spawn_first(
        &mut self,
        seg: Arc<Segment>,
        stream: ByteStream,
        ttfb: Duration,
    ) -> Option<WorkerId> {
        self.spawn_job(Work::Own(seg), Some((stream, ttfb)))
    }

    fn spawn_job(&mut self, work: Work, first: Option<First>) -> Option<WorkerId> {
        let wid = self.next_wid;
        self.next_wid += 1;
        let cancel = CancellationToken::new();
        let (seg, range, work_kind) = match work {
            Work::Own(seg) => {
                let r = seg.assign_owner(wid, cancel.clone());
                (seg, r, None)
            }
            Work::Hedge(seg, kind) => {
                let r = seg.assign_challenger(wid, kind, cancel.clone());
                (seg, r, Some(kind))
            }
        };
        let (start, end) = range?;
        let (Some(storage), Some(source)) = (self.storage.clone(), self.source.clone()) else {
            seg.release(wid);
            return None;
        };
        let req = OpenRequest {
            range: if self.ranged {
                ByteRange::new(start, end)
            } else {
                None
            },
            expect: self.expect.clone(),
        };
        tracing::debug!(id = %self.id, wid, start, end, hedge = work_kind.is_some(), first = first.is_some(), "connection opened");
        let counters = Arc::new(Counters::default());
        let mut started = Instant::now();
        let first = first.map(|(stream, ttfb)| {
            // Its headers were checked when `start` returned; what `start`
            // took is its time to first byte.
            let now = started;
            started = now.checked_sub(ttfb).unwrap_or(now);
            counters
                .opened_us
                .store(now_us_since(self.epoch, now).max(1), Ordering::Relaxed);
            stream
        });
        self.tasks.spawn(worker::work(Job {
            wid,
            seg: seg.clone(),
            source,
            storage,
            req,
            first,
            counters: counters.clone(),
            cancel: cancel.clone(),
            limiter: self.inner.limiter.clone(),
            epoch: self.epoch,
            coalesce: self.t.write_coalesce,
            coalesce_delay: self.t.write_coalesce_delay,
        }));
        self.workers.insert(
            wid,
            Wk {
                seg,
                counters,
                cancel,
                started,
                cancelling: false,
                last_bytes: 0,
                ewma: None,
                ttfb_seen: false,
            },
        );
        Some(wid)
    }

    fn on_exit(
        &mut self,
        joined: Result<(WorkerId, Exit), tokio::task::JoinError>,
    ) -> Result<(), Abort> {
        let (wid, exit) = match joined {
            Ok(v) => v,
            Err(e) if e.is_cancelled() => return Ok(()),
            Err(e) => return Err(panicked(e)),
        };
        let Some(w) = self.workers.remove(&wid) else {
            return Ok(());
        };
        tracing::debug!(id = %self.id, wid, ?exit, "connection closed");
        self.recv_total += w.counters.bytes.load(Ordering::Relaxed) - w.last_bytes;
        self.note_ttfb(&w);
        w.seg.release(wid);
        let alone = self
            .solo
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(&s.seg, &w.seg));
        match exit {
            Exit::Finished => {
                self.replacements += 1;
                Ok(())
            }
            Exit::Lost | Exit::Cancelled => Ok(()),
            Exit::Failed { err, streamed } => {
                // A reset after real progress is not the server pushing
                // back: the range is taken up again at once instead of
                // after the backoff (the failure round still counts, and a
                // server that resets right away still gets the backoff).
                // Likewise for the connection running alone: nothing else
                // is downloading.
                let delivered = w.counters.bytes.load(Ordering::Relaxed);
                let replace = streamed
                    && matches!(err, SourceError::Transient(_))
                    && (alone || delivered >= self.t.min_split_floor);
                self.on_source_error(err, streamed)?;
                if replace {
                    self.replacements += 1;
                }
                Ok(())
            }
            Exit::Storage(e) => Err(io_failed(e).into()),
        }
    }

    fn note_ttfb(&mut self, w: &Wk) {
        if !w.ttfb_seen
            && let Some(t) = ttfb_of(self.epoch, w)
        {
            self.push_ttfb(t);
        }
    }

    fn push_ttfb(&mut self, t: f64) {
        if self.ttfbs.len() >= 16 {
            self.ttfbs.pop_front();
        }
        self.ttfbs.push_back(t);
    }

    /// Sources classify, the engine decides (ADR 0005).
    fn on_source_error(&mut self, err: SourceError, streamed: bool) -> Result<(), Abort> {
        tracing::debug!(id = %self.id, "connection failed: {err}");
        match &err {
            SourceError::Transient(_) => {
                if !streamed {
                    // Refused or reset before the first byte while other
                    // connections work: most likely a connection cap.
                    let now = Instant::now();
                    self.open_failures.push_back(now);
                    while self
                        .open_failures
                        .front()
                        .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(2))
                    {
                        self.open_failures.pop_front();
                    }
                    let working = self.working_owners();
                    if self.open_failures.len() >= 2
                        && working > 0
                        && let Some(t) = self.tuner.as_mut()
                    {
                        t.on_throttle(working, false);
                    }
                }
                self.note_failure(&err, None)
            }
            SourceError::Throttled { retry_after, .. } => {
                let working = self.working_owners();
                let repeat = Instant::now() < self.open_gate;
                if let Some(t) = self.tuner.as_mut() {
                    t.on_throttle(working, repeat);
                }
                let ra = *retry_after;
                self.note_failure(&err, ra)
            }
            SourceError::LinkExpired(_) => Err(Outcome::NeedsRefresh(source_info(&err)).into()),
            SourceError::RemoteChanged => Err(Abort::Remote),
            SourceError::RangesUnsupported => Err(Abort::NoRanges),
            SourceError::Fatal(_) => Err(Outcome::Failed(source_info(&err)).into()),
        }
    }

    /// Count a failure round and push back new connections. Failures that
    /// arrive while already backing off belong to the same round.
    fn note_failure(
        &mut self,
        err: &SourceError,
        retry_after: Option<Duration>,
    ) -> Result<(), Abort> {
        let now = Instant::now();
        if now >= self.open_gate {
            self.consecutive += 1;
        }
        if self.consecutive > self.t.error_budget {
            return Err(Outcome::Failed(source_info(err)).into());
        }
        // `Retry-After` is honoured up to `retry_cap`: a server asking for a
        // day must not park the download outside the error budget.
        let delay = retry_after
            .unwrap_or_else(|| {
                backoff(
                    self.t.retry_base,
                    self.t.retry_cap,
                    self.consecutive,
                    &mut self.rng,
                )
            })
            .min(self.t.retry_cap);
        let until = now.checked_add(delay).unwrap_or(now + self.t.retry_cap);
        self.open_gate = self.open_gate.max(until);
        Ok(())
    }

    async fn on_tick(&mut self) -> Result<(), Abort> {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().max(1e-6);
        self.last_tick = now;
        let alpha = 1.0 - (-dt / self.t.ewma_tau.as_secs_f64().max(1e-3)).exp();
        let mut delta_total = 0u64;
        let epoch = self.epoch;
        let mut ttfb_new = Vec::new();
        for w in self.workers.values_mut() {
            let b = w.counters.bytes.load(Ordering::Relaxed);
            let delta = b - w.last_bytes;
            w.last_bytes = b;
            delta_total += delta;
            if w.first_byte() {
                let inst = delta as f64 / dt;
                w.ewma = Some(match w.ewma {
                    Some(e) => e + alpha * (inst - e),
                    None => inst,
                });
            }
            if !w.ttfb_seen
                && let Some(t) = ttfb_of(epoch, w)
            {
                w.ttfb_seen = true;
                ttfb_new.push(t);
            }
        }
        for t in ttfb_new {
            self.push_ttfb(t);
        }
        self.recv_total += delta_total;
        if delta_total > 0 {
            self.consecutive = 0;
        }
        let inst = delta_total as f64 / dt;
        self.speed += alpha * (inst - self.speed);
        let mut live: Vec<f64> = self
            .workers
            .values()
            .filter(|w| !w.cancelling)
            .filter_map(|w| w.ewma)
            .filter(|s| *s > 0.0)
            .collect();
        if live.len() >= 2 || self.per_conn_bps.is_none() {
            self.per_conn_bps = median(&mut live).or(self.per_conn_bps);
        }

        // Live stats for list/get/progress events.
        let written = self.written_total();
        self.live.downloaded.store(written, Ordering::Relaxed);
        self.live.speed.store(self.speed as u64, Ordering::Relaxed);
        self.live
            .connections
            .store(self.active() as u32, Ordering::Relaxed);
        if now.duration_since(self.last_segments) >= Duration::from_millis(200) {
            self.last_segments = now;
            self.publish();
        }

        if self.ranged {
            self.tune(now);
            self.hedge_stalls(now)?;
            let spacing = self.t.checkpoint_interval.max(
                self.ckpt_cost
                    .div_f64(self.t.checkpoint_share.max(1e-3))
                    .min(self.t.checkpoint_interval_max),
            );
            let due = now.duration_since(self.last_commit) >= spacing
                || written.saturating_sub(self.committed) >= self.t.checkpoint_bytes;
            if due && self.ckpt.is_none() {
                self.start_checkpoint();
            }
        }
        self.writeback();
        if now.duration_since(self.last_record) >= self.t.record_interval {
            self.last_record = now;
            let total = self.size;
            self.inner
                .update(&self.id, self.generation, |r| {
                    r.downloaded_bytes = written;
                    if total.is_some() {
                        r.total_bytes = total;
                    }
                })
                .await;
        }
        Ok(())
    }

    fn tune(&mut self, now: Instant) {
        let streaming = self.streaming_owners();
        let work_left: u64 = self.segs.iter().map(|s| s.snapshot().remaining()).sum();
        let max = self.inner.max_connections();
        let limited = self.inner.limiter.limited();
        let recv = self.recv_total;
        let ttfb = self.median_ttfb();
        let Some(tuner) = self.tuner.as_mut() else {
            return;
        };
        tuner.set_max(max);
        let before = tuner.target();
        tuner.tick(Sample {
            now,
            bytes: recv,
            streaming,
            work_left,
            limited,
            ttfb,
        });
        if tuner.target() != before {
            tracing::debug!(id = %self.id, from = before, to = tuner.target(), "connection target");
        }
        // Fewer connections wanted: cancel the slowest owners. Their ranges
        // become ownerless and are picked up by the others.
        let target = usize::from(tuner.target());
        let mut owners: Vec<(WorkerId, f64)> = self
            .workers
            .iter()
            .filter(|(wid, w)| !w.cancelling && w.seg.snapshot().owner == Some(**wid))
            .map(|(wid, w)| (*wid, w.ewma.unwrap_or(0.0)))
            .collect();
        if owners.len() > target {
            owners.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (wid, _) in owners.iter().take(owners.len() - target) {
                if let Some(w) = self.workers.get_mut(wid) {
                    w.cancelling = true;
                    w.cancel.cancel();
                }
            }
        }
    }

    /// Hedge connections that went silent, even above the target (up to
    /// `max_extra_hedges`): whichever of the two delivers first wins. A
    /// hedge that never delivers is cancelled (freeing the range for the
    /// next one); once a range's hedges are spent, its stalled owner is
    /// cancelled instead and the range handed to a fresh connection, as a
    /// failure round (so a server that stalls everything still ends in
    /// `failed`).
    fn hedge_stalls(&mut self, now: Instant) -> Result<(), Abort> {
        let limit_bytes = self.stall_limit();
        let limit_first = self.first_byte_limit();
        let now_us = now_us_since(self.epoch, now);
        let gated = now < self.open_gate;
        let mut room = (usize::from(self.target()) + usize::from(self.t.max_extra_hedges))
            .saturating_sub(self.active());
        let mut hedge = Vec::new();
        let mut give_up = Vec::new();
        let mut replace = Vec::new();
        for (wid, w) in &self.workers {
            if w.cancelling {
                continue;
            }
            let n = w.seg.snapshot();
            if n.remaining() == 0 {
                continue;
            }
            let last = w.counters.last_us.load(Ordering::Relaxed);
            let stalled = if last != 0 {
                Duration::from_micros(now_us.saturating_sub(last)) >= limit_bytes
            } else {
                now.duration_since(w.started) >= limit_first
            };
            if !stalled {
                continue;
            }
            if n.challenger == Some(*wid) {
                give_up.push(*wid);
            } else if n.owner == Some(*wid) && n.challenger.is_none() {
                if n.hedges < self.t.max_hedges_per_range {
                    if !gated && room > 0 {
                        room -= 1;
                        hedge.push(w.seg.clone());
                    }
                } else {
                    replace.push(*wid);
                }
            }
        }
        for wid in give_up.iter().chain(&replace) {
            if let Some(w) = self.workers.get_mut(wid) {
                w.cancelling = true;
                w.cancel.cancel();
            }
        }
        if !replace.is_empty() {
            tracing::debug!(id = %self.id, n = replace.len(), "replacing stalled connections");
            self.note_failure(&SourceError::Transient("connection stalled".into()), None)?;
        }
        for seg in hedge {
            tracing::debug!(id = %self.id, start = seg.start, "hedging a stalled connection");
            self.spawn(Work::Hedge(seg, HedgeKind::Stall));
        }
        Ok(())
    }

    fn written_total(&self) -> u64 {
        self.done.len()
            + self
                .segs
                .iter()
                .map(|s| s.written().saturating_sub(s.start))
                .sum::<u64>()
    }

    fn snapshot_done(&self) -> RangeSet {
        let mut d = self.done.clone();
        for s in &self.segs {
            if let Some(r) = ByteRange::new(s.start, s.written()) {
                d.insert(r);
            }
        }
        d
    }

    fn start_checkpoint(&mut self) {
        if !self.committable {
            return;
        }
        let Some(storage) = self.storage.clone() else {
            return;
        };
        let cp = Checkpoint {
            meta: self.meta.clone(),
            done: self.snapshot_done(),
            finishing: None,
        };
        self.committed = cp.done.len();
        self.last_commit = Instant::now();
        self.ckpt_started = self.last_commit;
        self.committed_any = true;
        self.ckpt_done = Some(cp.done.clone());
        self.flushed_upto = self.written_total();
        let journal = self.inner.deps.journal.clone();
        let id = self.id.clone();
        let durability = self.durability;
        self.ckpt = Some(Box::pin(async move {
            persist(&*storage, durability).await?;
            journal.commit(&id, &cp).await
        }));
    }

    /// While a transfer runs, flush every `writeback_bytes` of new data
    /// (and, when resumable, commit): the store syncs on a thread of its
    /// own, so the disk writes alongside the network instead of all at the
    /// end, when `finish` must flush before the rename.
    fn writeback(&mut self) {
        // Process-safe: nothing is forced to disk at the end either, so
        // there is nothing to spread out.
        if self.ckpt.is_some() || self.durability == Durability::ProcessSafe {
            return;
        }
        let written = self.written_total();
        if written.saturating_sub(self.flushed_upto) < self.t.writeback_bytes {
            return;
        }
        if self.committable && self.ranged {
            self.start_checkpoint();
        } else if let Some(storage) = self.storage.clone() {
            self.flushed_upto = written;
            self.ckpt_done = None;
            self.ckpt = Some(Box::pin(async move { storage.flush().await }));
        }
    }

    /// The checkpoint in flight resolved: on success the journal claims
    /// what it carried.
    fn ckpt_landed(&mut self, ok: bool) {
        let done = self.ckpt_done.take();
        if ok && let Some(d) = done {
            self.durable = d;
        }
    }

    async fn checkpoint_now(&mut self) -> io::Result<()> {
        let Some(storage) = self.storage.clone() else {
            return Ok(());
        };
        let cp = Checkpoint {
            meta: self.meta.clone(),
            done: self.snapshot_done(),
            finishing: None,
        };
        persist(&*storage, self.durability).await?;
        self.committed_any = true;
        self.inner.deps.journal.commit(&self.id, &cp).await?;
        self.durable = cp.done.clone();
        self.committed = cp.done.len();
        self.last_commit = Instant::now();
        Ok(())
    }

    /// Publish per-segment progress for `download.get` and events.
    fn publish(&self) {
        let infos = self.segment_infos();
        self.live
            .downloaded
            .store(self.written_total(), Ordering::Relaxed);
        *self.live.segments.lock().unwrap_or_else(|e| e.into_inner()) = infos;
    }
}

/// Make every resolved write as durable as `durability` promises before
/// the journal may claim it: forced to disk (`flush`), or handed to the
/// operating system (`write_out`: survives a killed process, not a power
/// cut).
fn persist(storage: &dyn Storage, durability: Durability) -> BoxFuture<'_, io::Result<()>> {
    match durability {
        Durability::PowerSafe => storage.flush(),
        Durability::ProcessSafe => storage.write_out(),
    }
}

/// Seconds from spawning a connection to its response headers.
fn ttfb_of(epoch: Instant, w: &Wk) -> Option<f64> {
    let opened = w.counters.opened_us.load(Ordering::Relaxed);
    if opened == 0 {
        return None;
    }
    let started = w.started.duration_since(epoch).as_micros() as u64;
    Some(opened.saturating_sub(started) as f64 / 1e6)
}

fn now_us_since(epoch: Instant, now: Instant) -> u64 {
    now.duration_since(epoch).as_micros() as u64
}

fn abort_outcome(a: Abort) -> Outcome {
    match a {
        Abort::Out(o) => o,
        Abort::Remote => Outcome::Failed(error_info("remote_changed", "remote file changed")),
        Abort::NoRanges => Outcome::Failed(error_info("ranges_unsupported", "ranges unsupported")),
    }
}

fn panicked(e: tokio::task::JoinError) -> Abort {
    tracing::error!("connection task failed: {e}");
    Abort::Out(Outcome::Failed(error_info("internal", e.to_string())))
}

/// What a re-fetched tail window says about the local copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tail {
    Match,
    Mismatch,
    /// Storage holds less than the journal claims (part file gone or
    /// truncated): local damage, whatever the validators say.
    LocalShort,
}

/// Compare what storage holds at `win` with a fresh fetch of it.
async fn compare(
    source: &dyn RangeSource,
    storage: &dyn Storage,
    expect: &Validators,
    win: ByteRange,
) -> Result<Tail, SourceError> {
    let local = match storage.read_at(win.start, win.len() as usize).await {
        Ok(b) if b.len() as u64 == win.len() => b,
        _ => return Ok(Tail::LocalShort),
    };
    let mut s = source
        .open(OpenRequest {
            range: Some(win),
            expect: expect.clone(),
        })
        .await?;
    let mut remote = Vec::with_capacity(win.len() as usize);
    while let Some(c) = s.next().await {
        remote.extend_from_slice(&c?);
    }
    if remote.len() as u64 != win.len() {
        return Err(SourceError::Transient("short tail response".into()));
    }
    Ok(if local[..] == remote[..] {
        Tail::Match
    } else {
        Tail::Mismatch
    })
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Driver").field("id", &self.id).finish()
    }
}
