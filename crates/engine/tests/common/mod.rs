//! Shared test doubles: a fault-injecting `RangeSource`, a lossy storage
//! that forgets unflushed writes on a simulated crash, and helpers.
//!
//! Everything runs on tokio's paused clock, so throttles, stalls and
//! time-to-first-byte are exact and tests take no wall-clock time.

#![allow(dead_code, missing_debug_implementations)]

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::future::BoxFuture;
use tokio::time::Instant;
use whoosh_core::testing::{MemJournal, MemSourceFactory, MemStateStore, MemStorageProvider};
use whoosh_core::{
    ByteStream, OpenRequest, Protocol, RangeSource, RemoteMeta, SourceError, Storage,
    StorageProvider, Validators,
};
use whoosh_engine::{Engine, EngineConfig, EngineDeps};
use whoosh_proto::service::Service;
use whoosh_proto::{AddParams, DownloadDetail, DownloadState, IdParams};

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * 1024;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Deterministic pseudo-random content, distinct per seed (so mixed
/// versions are detectable) and per 64 KiB block (so misplaced bytes are).
/// Built from one random block with memcpy: fast even in debug builds.
pub fn content(len: u64, seed: u64) -> Bytes {
    const B: usize = 64 * 1024;
    let mut block = vec![0u8; B + 4096];
    fastrand::Rng::with_seed(seed).fill(&mut block);
    let len = len as usize;
    let mut v = Vec::with_capacity(len);
    let mut i = 0u64;
    while v.len() < len {
        let rot = (i as usize).wrapping_mul(1031) % 4096;
        let take = B.min(len - v.len());
        let start = v.len();
        v.extend_from_slice(&block[rot..rot + take]);
        let stamp = (i ^ seed.rotate_left(17)).to_le_bytes();
        let n = stamp.len().min(take);
        v[start..start + n].copy_from_slice(&stamp[..n]);
        i += 1;
    }
    Bytes::from(v)
}

// --------------------------------------------------------------- source

/// What happens to one request (by 1-based open number).
#[derive(Debug, Clone, Default)]
pub struct ReqPlan {
    /// Extra time before the response headers.
    pub ttfb: Duration,
    /// Bytes per second for this response.
    pub rate: Option<u64>,
    /// Pause for `.1` once `.0` bytes of this response were sent.
    pub stall: Option<(u64, Duration)>,
    /// Reset after this many bytes of this response.
    pub reset_after: Option<u64>,
    /// Fail the request outright.
    pub fail: Option<SourceError>,
}

pub type Script = Arc<dyn Fn(u64, &OpenRequest) -> ReqPlan + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub enum Cap {
    /// 429/503 with this `Retry-After`.
    Throttle(Option<Duration>),
    /// Excess connections are reset.
    Reset,
}

#[derive(Clone)]
pub struct Cfg {
    pub chunk: usize,
    /// What the probe reports.
    pub accepts_ranges: bool,
    /// Whether ranged requests are really honoured.
    pub honours_ranges: bool,
    pub size_known: bool,
    pub validators: bool,
    pub per_conn_bps: Option<u64>,
    pub global_bps: Option<u64>,
    pub ttfb: Duration,
    pub cap: Option<(usize, Cap)>,
    /// Switch to the next version after this many bytes were served (once
    /// per version, while versions remain).
    pub change_after: Option<u64>,
    /// Links expire after this many bytes were served.
    pub expire_after: Option<u64>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    /// The size the probe claims, when it should lie.
    pub probe_size: Option<u64>,
    /// Slow start: a response begins at 1/16 of its rate and approaches it
    /// with this time constant.
    pub ramp: Duration,
    /// Multiplicative jitter of every chunk's pacing (uniform ±`noise`).
    pub noise: f64,
    /// `start` is one request (`GET` with `Range: bytes=0-`), as a network
    /// source does it; otherwise the trait's default (probe, then open).
    pub native_start: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            chunk: 64 * KIB as usize,
            accepts_ranges: true,
            honours_ranges: true,
            size_known: true,
            validators: true,
            per_conn_bps: None,
            global_bps: None,
            ttfb: Duration::ZERO,
            cap: None,
            change_after: None,
            expire_after: None,
            filename: None,
            content_type: None,
            probe_size: None,
            ramp: Duration::ZERO,
            noise: 0.0,
            native_start: false,
        }
    }
}

#[derive(Default)]
pub struct SrcState {
    pub version: usize,
    pub served: u64,
    pub since_change: u64,
    pub active: usize,
    pub peak: usize,
    pub opens: u64,
    pub expired: bool,
    pub ranges: Vec<Option<(u64, u64)>>,
    pub probe_failures: VecDeque<SourceError>,
    /// One-request `start`s (each also counts as an open).
    pub starts: u64,
    pub probes: u64,
}

pub struct Bucket {
    rate: f64,
    st: Mutex<(f64, Option<Instant>)>,
}

impl Bucket {
    pub fn new(rate: u64) -> Self {
        Self {
            rate: rate as f64,
            st: Mutex::new((0.0, None)),
        }
    }

    pub async fn take(&self, n: usize) {
        let wait = {
            let mut s = lock(&self.st);
            let now = Instant::now();
            let last = s.1.unwrap_or(now);
            let burst = (self.rate * 0.02).max(16.0 * 1024.0);
            s.0 = (s.0 + now.saturating_duration_since(last).as_secs_f64() * self.rate).min(burst);
            s.1 = Some(now);
            s.0 -= n as f64;
            (s.0 < 0.0).then(|| Duration::from_secs_f64(-s.0 / self.rate))
        };
        if let Some(w) = wait {
            tokio::time::sleep(w).await;
        }
    }
}

/// A `RangeSource` over in-memory versions of a file, with faults.
pub struct FaultSource {
    label: String,
    versions: Vec<Bytes>,
    pub cfg: Cfg,
    script: Option<Script>,
    global: Option<Arc<Bucket>>,
    pub st: Arc<Mutex<SrcState>>,
}

impl FaultSource {
    pub fn new(data: Bytes) -> Self {
        Self::with_versions(vec![data])
    }

    pub fn with_versions(versions: Vec<Bytes>) -> Self {
        Self::build(versions, Cfg::default(), None)
    }

    pub fn build(versions: Vec<Bytes>, cfg: Cfg, script: Option<Script>) -> Self {
        let global = cfg.global_bps.map(|r| Arc::new(Bucket::new(r)));
        Self {
            label: "fault".into(),
            versions,
            cfg,
            script,
            global,
            st: Arc::new(Mutex::new(SrcState::default())),
        }
    }

    pub fn cfg(mut self, f: impl FnOnce(&mut Cfg)) -> Self {
        f(&mut self.cfg);
        self.global = self.cfg.global_bps.map(|r| Arc::new(Bucket::new(r)));
        self
    }

    pub fn script(
        mut self,
        s: impl Fn(u64, &OpenRequest) -> ReqPlan + Send + Sync + 'static,
    ) -> Self {
        self.script = Some(Arc::new(s));
        self
    }

    pub fn opens(&self) -> u64 {
        lock(&self.st).opens
    }

    /// Requests of any kind: probes and opens (a native start is one open).
    pub fn requests(&self) -> u64 {
        let s = lock(&self.st);
        s.opens + s.probes
    }

    pub fn starts(&self) -> u64 {
        lock(&self.st).starts
    }

    pub fn peak(&self) -> usize {
        lock(&self.st).peak
    }

    pub fn served(&self) -> u64 {
        lock(&self.st).served
    }

    pub fn version(&self) -> usize {
        lock(&self.st).version
    }

    pub fn expire(&self) {
        lock(&self.st).expired = true;
    }

    pub fn fail_next_probe(&self, e: SourceError) {
        lock(&self.st).probe_failures.push_back(e);
    }

    pub fn data(&self, version: usize) -> Bytes {
        self.versions[version].clone()
    }

    fn validators_of(&self, v: usize) -> Validators {
        Validators {
            etag: self.cfg.validators.then(|| format!("\"v{}\"", v + 1)),
            last_modified: None,
            size: self.cfg.size_known.then_some(self.versions[v].len() as u64),
        }
    }
}

struct ActiveGuard(Arc<Mutex<SrcState>>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        lock(&self.0).active -= 1;
    }
}

struct Body {
    data: Bytes,
    pos: usize,
    born: Instant,
    ramp: Duration,
    noise: f64,
    rng: fastrand::Rng,
    chunk: usize,
    rate: Option<u64>,
    global: Option<Arc<Bucket>>,
    plan: ReqPlan,
    stalled: bool,
    shared: Arc<Mutex<SrcState>>,
    change_after: Option<u64>,
    expire_after: Option<u64>,
    versions: usize,
    done: bool,
    _guard: ActiveGuard,
}

async fn next_chunk(mut b: Body) -> Option<(Result<Bytes, SourceError>, Body)> {
    if b.done || b.pos >= b.data.len() {
        return None;
    }
    if let Some((after, dur)) = b.plan.stall
        && !b.stalled
        && b.pos as u64 >= after
    {
        b.stalled = true;
        tokio::time::sleep(dur).await;
    }
    if let Some(after) = b.plan.reset_after
        && b.pos as u64 >= after
    {
        b.done = true;
        return Some((Err(SourceError::Transient("connection reset".into())), b));
    }
    let n = b.chunk.min(b.data.len() - b.pos);
    if let Some(r) = b.rate {
        let mut rate = r as f64;
        if !b.ramp.is_zero() {
            let t = b.born.elapsed().as_secs_f64() / b.ramp.as_secs_f64();
            rate *= 1.0 - (15.0 / 16.0) * (-t).exp();
        }
        let jitter = 1.0 + b.noise * (2.0 * b.rng.f64() - 1.0);
        tokio::time::sleep(Duration::from_secs_f64(n as f64 / rate * jitter)).await;
    }
    if let Some(g) = &b.global {
        g.take(n).await;
    }
    let c = b.data.slice(b.pos..b.pos + n);
    b.pos += n;
    {
        let mut s = lock(&b.shared);
        s.served += n as u64;
        s.since_change += n as u64;
        if let Some(after) = b.change_after
            && s.since_change >= after
            && s.version + 1 < b.versions
        {
            s.version += 1;
            s.since_change = 0;
        }
        if b.expire_after.is_some_and(|e| s.served >= e) {
            s.expired = true;
        }
    }
    Some((Ok(c), b))
}

impl RangeSource for FaultSource {
    fn label(&self) -> &str {
        &self.label
    }

    fn probe(&self) -> BoxFuture<'_, Result<RemoteMeta, SourceError>> {
        Box::pin(async move {
            lock(&self.st).probes += 1;
            if !self.cfg.ttfb.is_zero() {
                tokio::time::sleep(self.cfg.ttfb).await;
            }
            let mut s = lock(&self.st);
            if let Some(e) = s.probe_failures.pop_front() {
                return Err(e);
            }
            if s.expired {
                return Err(SourceError::LinkExpired("403".into()));
            }
            Ok(self.meta_of(s.version, self.cfg.accepts_ranges))
        })
    }

    fn open(&self, req: OpenRequest) -> BoxFuture<'_, Result<ByteStream, SourceError>> {
        Box::pin(async move { self.respond(req, false).await.map(|(_, b)| b) })
    }

    fn start(&self) -> BoxFuture<'_, Result<(RemoteMeta, ByteStream), SourceError>> {
        if !self.cfg.native_start {
            // The trait's default: probe, then open the whole file.
            return Box::pin(async move {
                let meta = self.probe().await?;
                let stream = self
                    .open(OpenRequest {
                        range: None,
                        expect: meta.validators(),
                    })
                    .await?;
                Ok((meta, stream))
            });
        }
        Box::pin(async move {
            lock(&self.st).starts += 1;
            let req = OpenRequest::default();
            self.respond(req, true).await
        })
    }
}

impl FaultSource {
    fn meta_of(&self, version: usize, accepts_ranges: bool) -> RemoteMeta {
        let v = self.validators_of(version);
        RemoteMeta {
            size: self.cfg.probe_size.or(v.size),
            accepts_ranges,
            etag: v.etag,
            last_modified: None,
            filename: self.cfg.filename.clone(),
            content_type: self.cfg.content_type.clone(),
            final_url: None,
            protocol: Some(Protocol::Http1),
        }
    }

    /// One request. `start`: the whole file from offset 0, described by
    /// the same response (`Range: bytes=0-`; a `206` proves ranges work, a
    /// server that ignores them answers `200`).
    async fn respond(
        &self,
        req: OpenRequest,
        start: bool,
    ) -> Result<(RemoteMeta, ByteStream), SourceError> {
        {
            let n = {
                let mut s = lock(&self.st);
                s.opens += 1;
                s.ranges.push(req.range.map(|r| (r.start, r.end)));
                s.opens
            };
            let plan = self.script.as_ref().map(|f| f(n, &req)).unwrap_or_default();
            let ttfb = self.cfg.ttfb + plan.ttfb;
            if !ttfb.is_zero() {
                tokio::time::sleep(ttfb).await;
            }
            if let Some(e) = plan.fail.clone() {
                return Err(e);
            }
            let mut meta = RemoteMeta::default();
            let body = {
                let mut s = lock(&self.st);
                if start && let Some(e) = s.probe_failures.pop_front() {
                    return Err(e);
                }
                if s.expired {
                    return Err(SourceError::LinkExpired("403".into()));
                }
                if start {
                    meta = self.meta_of(
                        s.version,
                        self.cfg.accepts_ranges && self.cfg.honours_ranges,
                    );
                }
                if let Some((max, cap)) = self.cfg.cap
                    && s.active >= max
                {
                    return Err(match cap {
                        Cap::Throttle(ra) => SourceError::Throttled {
                            message: "503".into(),
                            retry_after: ra,
                        },
                        Cap::Reset => SourceError::Transient("connection reset".into()),
                    });
                }
                let v = s.version;
                if req.expect.conflicts_with(&self.validators_of(v)) {
                    return Err(SourceError::RemoteChanged);
                }
                let data = &self.versions[v];
                let body = match req.range {
                    Some(r) => {
                        if !self.cfg.honours_ranges {
                            return Err(SourceError::RangesUnsupported);
                        }
                        if r.end > data.len() as u64 || r.is_empty() {
                            return Err(SourceError::RemoteChanged);
                        }
                        data.slice(r.start as usize..r.end as usize)
                    }
                    None => data.clone(),
                };
                s.active += 1;
                s.peak = s.peak.max(s.active);
                body
            };
            let b = Body {
                data: body,
                pos: 0,
                born: Instant::now(),
                ramp: self.cfg.ramp,
                noise: self.cfg.noise,
                rng: fastrand::Rng::with_seed(n),
                chunk: self.cfg.chunk,
                rate: plan.rate.or(self.cfg.per_conn_bps),
                global: self.global.clone(),
                plan,
                stalled: false,
                shared: self.st.clone(),
                change_after: self.cfg.change_after,
                expire_after: self.cfg.expire_after,
                versions: self.versions.len(),
                done: false,
                _guard: ActiveGuard(self.st.clone()),
            };
            Ok((meta, futures::stream::unfold(b, next_chunk).boxed()))
        }
    }
}

// --------------------------------------------------------------- crash fs

/// Storage that keeps a volatile and a durable copy of every file: writes
/// go to the volatile copy, `flush` copies it to the durable one, and
/// [`CrashFs::crash`] throws away everything not flushed — a file created
/// and never flushed too (its directory entry was never synced). Every
/// operation yields once, so a kill can land between any two of them.
pub struct CrashFs {
    files: Arc<Mutex<HashMap<PathBuf, Arc<CrashFile>>>>,
    finished: Arc<Mutex<HashMap<PathBuf, Bytes>>>,
    /// `preallocate` leaves exactly `len` bytes (the contract). Off models
    /// a store that only grows files, to check the engine does not rely on
    /// truncation.
    pub truncate: bool,
    /// Freeze `finish` at a chosen point, to kill the process exactly there.
    pub hooks: Arc<FinishHooks>,
}

/// Where `finish` stops forever (the test then kills the engine).
#[derive(Default)]
pub struct FinishHooks {
    pub before_rename: std::sync::atomic::AtomicBool,
    pub after_rename: std::sync::atomic::AtomicBool,
    /// Set once `finish` reached a hook.
    pub hit: std::sync::atomic::AtomicBool,
}

impl FinishHooks {
    pub fn clear(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.before_rename.store(false, Relaxed);
        self.after_rename.store(false, Relaxed);
        self.hit.store(false, Relaxed);
    }

    pub fn hit(&self) -> bool {
        self.hit.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl CrashFs {
    /// A store whose `preallocate` only grows files.
    pub fn growing() -> Self {
        Self {
            truncate: false,
            ..Self::default()
        }
    }
}

impl Default for CrashFs {
    fn default() -> Self {
        Self {
            files: Arc::default(),
            finished: Arc::default(),
            truncate: true,
            hooks: Arc::default(),
        }
    }
}

pub struct CrashFile {
    path: PathBuf,
    truncate: bool,
    hooks: Arc<FinishHooks>,
    done: std::sync::atomic::AtomicBool,
    /// Its directory entry is on disk: it existed before it was opened, or
    /// a flush synced it. A power cut removes a file without one.
    entry: std::sync::atomic::AtomicBool,
    /// What the process wrote (lost when it is killed).
    volatile: Mutex<Vec<u8>>,
    /// What the operating system holds (`write_out`, `flush`): survives a
    /// killed process, not a power cut.
    os: Mutex<Vec<u8>>,
    /// What is on disk (`flush`): survives a power cut.
    durable: Mutex<Vec<u8>>,
    files: Arc<Mutex<HashMap<PathBuf, Arc<CrashFile>>>>,
    finished: Arc<Mutex<HashMap<PathBuf, Bytes>>>,
}

impl CrashFs {
    /// Power cut: every unflushed write is lost, and every file whose
    /// directory entry was never synced.
    pub fn crash(&self) {
        let mut files = lock(&self.files);
        files.retain(|_, f| f.entry.load(std::sync::atomic::Ordering::Relaxed));
        for f in files.values() {
            let d = lock(&f.durable).clone();
            *lock(&f.os) = d.clone();
            *lock(&f.volatile) = d;
        }
    }

    /// The process is killed: what it had not handed to the operating
    /// system is lost; what it had survives, flushed or not. (Finished
    /// files are the operating system's, so they stay too.)
    pub fn kill(&self) {
        for f in lock(&self.files).values() {
            let o = lock(&f.os).clone();
            *lock(&f.volatile) = o;
        }
    }

    pub fn finished(&self) -> HashMap<PathBuf, Bytes> {
        lock(&self.finished).clone()
    }

    /// What an unfinished file holds now (after `crash` or `kill`: what
    /// survived).
    pub fn contents(&self, path: &Path) -> Option<Bytes> {
        lock(&self.files)
            .get(path)
            .map(|f| Bytes::from(lock(&f.volatile).clone()))
    }

    /// Put a finished file in place (another program's, or a tampered one).
    pub fn put_finished(&self, path: &Path, bytes: Bytes) {
        lock(&self.finished).insert(path.to_path_buf(), bytes);
    }
}

impl Storage for CrashFile {
    fn preallocate(&self, len: u64) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            let mut v = lock(&self.volatile);
            if self.truncate || (v.len() as u64) < len {
                v.resize(len as usize, 0);
            }
            // A system call, after every earlier write (queue order).
            *lock(&self.os) = v.clone();
            Ok(())
        })
    }

    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            if self.done.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(io::Error::other("storage already finished"));
            }
            let mut v = lock(&self.volatile);
            let end = offset as usize + data.len();
            if v.len() < end {
                v.resize(end, 0);
            }
            v[offset as usize..end].copy_from_slice(&data);
            Ok(())
        })
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move {
            let v = lock(&self.volatile);
            let s = (offset as usize).min(v.len());
            let e = (s + len).min(v.len());
            Ok(Bytes::copy_from_slice(&v[s..e]))
        })
    }

    fn flush(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            let v = lock(&self.volatile).clone();
            *lock(&self.os) = v.clone();
            *lock(&self.durable) = v;
            self.entry.store(true, std::sync::atomic::Ordering::Relaxed);
            tokio::task::yield_now().await;
            Ok(())
        })
    }

    fn write_out(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            let v = lock(&self.volatile).clone();
            *lock(&self.os) = v;
            tokio::task::yield_now().await;
            Ok(())
        })
    }

    fn finish_unsynced<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        self.rename(final_path, false)
    }

    fn finish<'a>(&'a self, final_path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        self.rename(final_path, true)
    }
}

impl CrashFile {
    /// `finish` (`synced`) or `finish_unsynced`: queued writes reach the
    /// operating system first either way; only a synced finish makes them
    /// durable. (A power cut after an unsynced finish is not modelled.)
    fn rename<'a>(&'a self, final_path: &'a Path, synced: bool) -> BoxFuture<'a, io::Result<()>> {
        use std::sync::atomic::Ordering::Relaxed;
        Box::pin(async move {
            tokio::task::yield_now().await;
            if self.hooks.before_rename.load(Relaxed) {
                self.hooks.hit.store(true, Relaxed);
                std::future::pending::<()>().await;
            }
            {
                let mut done = lock(&self.finished);
                if done.contains_key(final_path) {
                    return Err(io::Error::from(io::ErrorKind::AlreadyExists));
                }
                // Rename: the part file is gone, the final file is the
                // operating system's (and durable, when synced).
                let v = lock(&self.volatile).clone();
                *lock(&self.os) = v.clone();
                if synced {
                    *lock(&self.durable) = v.clone();
                }
                done.insert(final_path.to_path_buf(), Bytes::from(v));
                lock(&self.files).remove(&self.path);
                self.done.store(true, Relaxed);
            }
            if self.hooks.after_rename.load(Relaxed) {
                self.hooks.hit.store(true, Relaxed);
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
}

impl StorageProvider for CrashFs {
    fn open<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            // Like a real file system: opening an existing (even finished)
            // file keeps its contents.
            let existing = lock(&self.finished).get(path).map(|b| b.to_vec());
            let f = lock(&self.files)
                .entry(path.to_path_buf())
                .or_insert_with(|| {
                    let entry = existing.is_some();
                    let init = existing.unwrap_or_default();
                    Arc::new(CrashFile {
                        path: path.to_path_buf(),
                        truncate: self.truncate,
                        hooks: self.hooks.clone(),
                        done: std::sync::atomic::AtomicBool::new(false),
                        entry: std::sync::atomic::AtomicBool::new(entry),
                        volatile: Mutex::new(init.clone()),
                        os: Mutex::new(init.clone()),
                        durable: Mutex::new(init),
                        files: self.files.clone(),
                        finished: self.finished.clone(),
                    })
                })
                .clone();
            Ok(f as Arc<dyn Storage>)
        })
    }

    fn file_len<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(async move {
            if let Some(f) = lock(&self.files).get(path) {
                return Ok(Some(lock(&f.volatile).len() as u64));
            }
            Ok(lock(&self.finished).get(path).map(|b| b.len() as u64))
        })
    }

    fn remove<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            lock(&self.files).remove(path);
            lock(&self.finished).remove(path);
            Ok(())
        })
    }
}

// --------------------------------------------------------------- harness

pub const URL: &str = "https://lab.test/files/data.bin";
pub const DIR: &str = "/dl";

pub fn config() -> EngineConfig {
    let mut c = EngineConfig::new(DIR);
    c.rng_seed = Some(7);
    c
}

pub struct Lab {
    pub engine: Arc<Engine>,
    pub sources: Arc<MemSourceFactory>,
    pub storage: Arc<MemStorageProvider>,
    pub journal: Arc<MemJournal>,
    pub state: Arc<MemStateStore>,
}

impl Lab {
    pub async fn new(cfg: EngineConfig) -> Self {
        let sources = Arc::new(MemSourceFactory::default());
        let storage = Arc::new(MemStorageProvider::default());
        let journal = Arc::new(MemJournal::default());
        let state = Arc::new(MemStateStore::default());
        let engine = Engine::start(
            EngineDeps {
                sources: sources.clone(),
                storage: storage.clone(),
                journal: journal.clone(),
                state: state.clone(),
            },
            cfg,
        )
        .await
        .unwrap();
        Self {
            engine,
            sources,
            storage,
            journal,
            state,
        }
    }

    pub fn deps(&self) -> EngineDeps {
        EngineDeps {
            sources: self.sources.clone(),
            storage: self.storage.clone(),
            journal: self.journal.clone(),
            state: self.state.clone(),
        }
    }

    pub fn serve(&self, url: &str, src: Arc<FaultSource>) -> Arc<FaultSource> {
        self.sources.insert(url, src.clone());
        src
    }

    pub async fn add(&self, url: &str) -> String {
        self.engine.add(add_params(url)).await.unwrap().id
    }

    pub fn file(&self, name: &str) -> Option<Bytes> {
        self.storage.finished(&Path::new(DIR).join(name))
    }
}

pub fn add_params(url: &str) -> AddParams {
    AddParams {
        url: url.into(),
        request: None,
        filename: None,
        directory: None,
        category: None,
        start: true,
        mirrors: vec![],
        checksum: None,
        connections: None,
        queue: None,
    }
}

pub async fn detail(engine: &Engine, id: &str) -> DownloadDetail {
    engine.get(IdParams { id: id.into() }).await.unwrap()
}

/// Polls (on the paused clock) until the download reaches one of `states`.
/// Panics after `limit` of virtual time.
pub async fn wait_for(
    engine: &Engine,
    id: &str,
    states: &[DownloadState],
    limit: Duration,
) -> DownloadDetail {
    let t0 = Instant::now();
    loop {
        let d = detail(engine, id).await;
        if states.contains(&d.summary.state) {
            return d;
        }
        assert!(
            t0.elapsed() < limit,
            "download {id} still {:?} after {limit:?} ({:?})",
            d.summary.state,
            d.summary.error
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Waits for `Completed` and returns the virtual time it took.
pub async fn finish(engine: &Engine, id: &str, limit: Duration) -> (DownloadDetail, Duration) {
    let t0 = Instant::now();
    let d = wait_for(
        engine,
        id,
        &[DownloadState::Completed, DownloadState::Failed],
        limit,
    )
    .await;
    assert_eq!(
        d.summary.state,
        DownloadState::Completed,
        "failed: {:?}",
        d.summary.error
    );
    (d, t0.elapsed())
}

/// Reads every byte of a stream (for assertions on sources).
pub async fn read_all(mut s: ByteStream) -> Result<Vec<u8>, SourceError> {
    let mut v = Vec::new();
    while let Some(c) = s.next().await {
        v.extend_from_slice(&c?);
    }
    Ok(v)
}

pub fn assert_same(got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "length differs");
    if got != want {
        let i = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or(0);
        panic!("content differs first at byte {i}");
    }
}

/// Log to the test output (`RUST_LOG=whoosh_engine=debug`).
pub fn trace() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

/// A `StateStore` whose record and host-profile saves take time, to widen
/// race windows deterministically on the paused clock.
pub struct SlowState {
    pub inner: MemStateStore,
    pub save_delay: Mutex<Duration>,
    pub profile_delay: Mutex<Duration>,
    /// `sync()` calls, whether they fail, and how many found a completed
    /// record already saved.
    pub syncs: std::sync::atomic::AtomicU64,
    pub fail_sync: std::sync::atomic::AtomicBool,
    pub syncs_after_completed: std::sync::atomic::AtomicU64,
}

impl SlowState {
    pub fn new() -> Self {
        Self {
            inner: MemStateStore::default(),
            save_delay: Mutex::new(Duration::ZERO),
            profile_delay: Mutex::new(Duration::ZERO),
            syncs: Default::default(),
            fail_sync: Default::default(),
            syncs_after_completed: Default::default(),
        }
    }
}

impl whoosh_core::StateStore for SlowState {
    fn load_downloads(&self) -> BoxFuture<'_, io::Result<Vec<whoosh_core::DownloadRecord>>> {
        self.inner.load_downloads()
    }
    fn save_download<'a>(
        &'a self,
        record: &'a whoosh_core::DownloadRecord,
    ) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let d = *lock(&self.save_delay);
            if !d.is_zero() {
                tokio::time::sleep(d).await;
            }
            self.inner.save_download(record).await
        })
    }
    fn delete_downloads<'a>(&'a self, ids: &'a [String]) -> BoxFuture<'a, io::Result<()>> {
        self.inner.delete_downloads(ids)
    }
    fn get_value<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<String>>> {
        self.inner.get_value(key)
    }
    fn set_value<'a>(&'a self, key: &'a str, json: &'a str) -> BoxFuture<'a, io::Result<()>> {
        self.inner.set_value(key, json)
    }
    fn host_profile<'a>(
        &'a self,
        host: &'a str,
    ) -> BoxFuture<'a, io::Result<Option<whoosh_core::HostProfile>>> {
        self.inner.host_profile(host)
    }
    fn sync(&self) -> BoxFuture<'_, io::Result<()>> {
        use std::sync::atomic::Ordering::Relaxed;
        Box::pin(async move {
            self.syncs.fetch_add(1, Relaxed);
            let completed = self
                .inner
                .load_downloads()
                .await?
                .iter()
                .any(|r| r.state == whoosh_proto::DownloadState::Completed);
            if completed {
                self.syncs_after_completed.fetch_add(1, Relaxed);
            }
            if self.fail_sync.load(Relaxed) {
                return Err(io::Error::other("fsync failed"));
            }
            Ok(())
        })
    }
    fn save_host_profile<'a>(
        &'a self,
        profile: &'a whoosh_core::HostProfile,
    ) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let d = *lock(&self.profile_delay);
            if !d.is_zero() {
                tokio::time::sleep(d).await;
            }
            self.inner.save_host_profile(profile).await
        })
    }
}

/// An engine over `SlowState` (plus the usual in-memory parts).
pub async fn slow_lab(cfg: EngineConfig) -> (Lab, Arc<SlowState>) {
    let sources = Arc::new(MemSourceFactory::default());
    let storage = Arc::new(MemStorageProvider::default());
    let journal = Arc::new(MemJournal::default());
    let slow = Arc::new(SlowState::new());
    let engine = Engine::start(
        EngineDeps {
            sources: sources.clone(),
            storage: storage.clone(),
            journal: journal.clone(),
            state: slow.clone(),
        },
        cfg,
    )
    .await
    .unwrap();
    (
        Lab {
            engine,
            sources,
            storage,
            journal,
            state: Arc::new(MemStateStore::default()),
        },
        slow,
    )
}

/// A source whose probe panics (to check a panicking run still ends).
pub struct PanicSource;

impl RangeSource for PanicSource {
    fn label(&self) -> &str {
        "panic"
    }
    fn probe(&self) -> BoxFuture<'_, Result<RemoteMeta, SourceError>> {
        Box::pin(async { panic!("probe exploded") })
    }
    fn open(&self, _req: OpenRequest) -> BoxFuture<'_, Result<ByteStream, SourceError>> {
        Box::pin(async { panic!("open exploded") })
    }
}
