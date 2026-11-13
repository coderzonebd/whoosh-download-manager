//! CPU cost of the engine's data path: push 1 GiB through the engine
//! in-process with 1, 4 and 16 connections, and report throughput and CPU
//! seconds per GB.
//!
//! The source serves zero-copy slices of a repeating pattern and the
//! storage discards writes (counting them), so the numbers are the
//! engine's own cost: scheduling, claims, counters, the per-write
//! `BoxFuture`, task wake-ups. Network and disk come on top.
//!
//! Storage modes: `null` discards writes inline; `thread` models
//! `whoosh-store`: every `write_at` is a message to a writer thread (a futex
//! wake when it sleeps) that issues one `pwrite` into a real file (page
//! cache; `$TMPDIR`, removed afterwards), through a bounded 8 MiB buffer
//! like the store's (`write_at` waits when it is full; `flush` and `finish`
//! wait until the writer caught up). The report includes the number of
//! `write_at` calls per GiB. `WHOOSH_COALESCE=<bytes>` overrides
//! `Tuning::write_coalesce` (0 = one write per network chunk, the old path).
//!
//! ```text
//! cargo run --release -p whoosh-engine --example throughput -- [GiB] [chunk KiB] [runs] [null|thread] [MB/s per connection] [connections, e.g. 1,4,16]
//! ```
//!
//! With a per-connection rate the source paces its chunks like a network
//! (then the writer thread drains faster than chunks arrive, as on a real
//! link, and every write wakes it).

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::future::BoxFuture;
use whoosh_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use whoosh_core::{
    ByteStream, OpenRequest, Protocol, RangeSource, RemoteMeta, SourceError, Storage,
    StorageProvider,
};
use whoosh_engine::{Engine, EngineConfig, EngineDeps, Tuning};
use whoosh_proto::service::Service;
use whoosh_proto::{AddParams, DownloadState, IdParams};

const PATTERN: usize = 4 << 20;

struct PatternSource {
    size: u64,
    chunk: usize,
    pattern: Bytes,
    /// Pause between chunks (pacing), if any.
    gap: Option<Duration>,
}

impl RangeSource for PatternSource {
    fn label(&self) -> &str {
        "pattern"
    }

    fn probe(&self) -> BoxFuture<'_, Result<RemoteMeta, SourceError>> {
        let meta = RemoteMeta {
            size: Some(self.size),
            accepts_ranges: true,
            etag: Some("\"p\"".into()),
            protocol: Some(Protocol::Other),
            ..RemoteMeta::default()
        };
        Box::pin(async move { Ok(meta) })
    }

    fn open(&self, req: OpenRequest) -> BoxFuture<'_, Result<ByteStream, SourceError>> {
        let (start, end) = req.range.map_or((0, self.size), |r| (r.start, r.end));
        let pattern = self.pattern.clone();
        let chunk = self.chunk as u64;
        let gap = self.gap;
        let s = futures::stream::unfold(start, move |pos| {
            let pattern = pattern.clone();
            async move {
                if pos >= end {
                    return None;
                }
                if let Some(g) = gap {
                    tokio::time::sleep(g).await;
                }
                // Never straddle the pattern's end: always a zero-copy slice.
                let off = (pos % PATTERN as u64) as usize;
                let n = chunk.min(end - pos).min((PATTERN - off) as u64) as usize;
                Some((Ok(pattern.slice(off..off + n)), pos + n as u64))
            }
        });
        Box::pin(async move { Ok(Box::pin(s) as ByteStream) })
    }
}

const BUFFER: usize = 8 << 20;

#[derive(Default)]
struct NullStorage {
    written: AtomicU64,
    writes: AtomicU64,
    writer: Option<std::sync::Mutex<std::sync::mpsc::Sender<(u64, Bytes)>>>,
    /// Free bytes of the bounded buffer (threaded mode).
    room: Option<Arc<tokio::sync::Semaphore>>,
}

impl NullStorage {
    async fn drained(&self) {
        if let Some(room) = &self.room {
            let all = room.acquire_many(BUFFER as u32).await.expect("open");
            drop(all);
        }
    }
}

impl NullStorage {
    #[cfg(unix)]
    fn threaded() -> Self {
        use std::os::unix::fs::FileExt;
        let (tx, rx) = std::sync::mpsc::channel::<(u64, Bytes)>();
        let room = Arc::new(tokio::sync::Semaphore::new(BUFFER));
        let freed = room.clone();
        let path = std::env::temp_dir().join(format!("whoosh-throughput-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("temp file");
        let _ = std::fs::remove_file(&path); // unlinked: gone when closed
        std::thread::spawn(move || {
            while let Ok((offset, data)) = rx.recv() {
                let _ = file.write_at(&data, offset);
                freed.add_permits(data.len().min(BUFFER));
            }
        });
        Self {
            writer: Some(std::sync::Mutex::new(tx)),
            room: Some(room),
            ..Self::default()
        }
    }

    #[cfg(not(unix))]
    fn threaded() -> Self {
        Self::default()
    }
}

impl Storage for NullStorage {
    fn preallocate(&self, _len: u64) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn write_at(&self, offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>> {
        self.written.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.writes.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            if let (Some(tx), Some(room)) = (&self.writer, &self.room) {
                let n = data.len().min(BUFFER) as u32;
                room.acquire_many(n).await.expect("open").forget();
                let _ = tx.lock().unwrap().send((offset, data));
            }
            Ok(())
        })
    }
    fn read_at(&self, _offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move { Ok(Bytes::from(vec![0; len])) })
    }
    fn flush(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async {
            self.drained().await;
            Ok(())
        })
    }
    fn finish<'a>(&'a self, _p: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async {
            self.drained().await;
            Ok(())
        })
    }
}

#[derive(Default)]
struct NullProvider {
    threaded: bool,
    last: std::sync::Mutex<Option<Arc<NullStorage>>>,
}

impl StorageProvider for NullProvider {
    fn open<'a>(&'a self, _p: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        let s = Arc::new(if self.threaded {
            NullStorage::threaded()
        } else {
            NullStorage::default()
        });
        *self.last.lock().unwrap() = Some(s.clone());
        Box::pin(async move { Ok(s as Arc<dyn Storage>) })
    }
    fn file_len<'a>(&'a self, _p: &'a Path) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(async { Ok(None) })
    }
    fn remove<'a>(&'a self, _p: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(unix)]
fn cpu_seconds() -> f64 {
    use nix::sys::resource::{UsageWho, getrusage};
    let u = getrusage(UsageWho::RUSAGE_SELF).expect("getrusage");
    let t = |tv: nix::sys::time::TimeVal| tv.tv_sec() as f64 + tv.tv_usec() as f64 / 1e6;
    t(u.user_time()) + t(u.system_time())
}

#[cfg(not(unix))]
fn cpu_seconds() -> f64 {
    f64::NAN
}

async fn one(size: u64, chunk: usize, conns: u16, threaded: bool, pace: f64) -> (f64, f64, u64) {
    let mut pattern = vec![0u8; PATTERN];
    fastrand::Rng::with_seed(1).fill(&mut pattern);
    let sources = Arc::new(MemSourceFactory::default());
    let url = "bench://pattern/1g.bin";
    sources.insert(
        url,
        Arc::new(PatternSource {
            size,
            chunk,
            pattern: Bytes::from(pattern),
            gap: (pace > 0.0).then(|| Duration::from_secs_f64(chunk as f64 / (pace * 1e6))),
        }),
    );
    let storage = Arc::new(NullProvider {
        threaded,
        ..NullProvider::default()
    });
    let mut cfg = EngineConfig::new("/bench");
    cfg.tuning = Tuning::default();
    if let Some(c) = std::env::var("WHOOSH_COALESCE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        cfg.tuning.write_coalesce = c;
    }
    let engine = Engine::start(
        EngineDeps {
            sources,
            storage: storage.clone(),
            journal: Arc::new(MemJournal::default()),
            state: Arc::new(MemStateStore::default()),
        },
        cfg,
    )
    .await
    .unwrap();
    let cpu0 = cpu_seconds();
    let t0 = Instant::now();
    let id = engine
        .add(AddParams {
            url: url.into(),
            request: None,
            filename: None,
            directory: None,
            category: None,
            start: true,
            mirrors: vec![],
            checksum: None,
            connections: Some(u32::from(conns)),
            queue: None,
        })
        .await
        .unwrap()
        .id;
    loop {
        let d = engine.get(IdParams { id: id.clone() }).await.unwrap();
        match d.summary.state {
            DownloadState::Completed => break,
            DownloadState::Failed => panic!("{:?}", d.summary.error),
            _ => tokio::time::sleep(Duration::from_millis(1)).await,
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu0;
    let (written, writes) = storage.last.lock().unwrap().as_ref().map_or((0, 0), |s| {
        (
            s.written.load(Ordering::Relaxed),
            s.writes.load(Ordering::Relaxed),
        )
    });
    assert_eq!(written, size, "every byte written exactly once");
    engine.shutdown().await;
    (wall, cpu, writes)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gib: f64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(1.0);
    let chunk_kib: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(64);
    let runs: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(3);
    let threaded = args.get(3).is_some_and(|m| m == "thread");
    let pace: f64 = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(0.0);
    let counts: Vec<u16> = args
        .get(5)
        .map(|a| a.split(',').filter_map(|c| c.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 4, 16]);
    let size = (gib * (1u64 << 30) as f64) as u64;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    println!(
        "engine data path, {gib} GiB, {chunk_kib} KiB chunks, {runs} runs (GiB/s and wall: best; CPU: median), storage {}, {} (4 worker threads)",
        if threaded { "thread" } else { "null" },
        if pace > 0.0 {
            format!("paced at {pace} MB/s per connection")
        } else {
            "unpaced".to_string()
        }
    );
    println!("| connections | GiB/s | CPU s per GB | write_at per GiB | wall s |");
    println!("|---:|---:|---:|---:|---:|");
    for conns in counts {
        let mut walls = Vec::new();
        let mut cpus = Vec::new();
        let mut writes = 0;
        for _ in 0..runs {
            let (wall, cpu, w) = rt.block_on(one(size, chunk_kib * 1024, conns, threaded, pace));
            walls.push(wall);
            cpus.push(cpu);
            writes = w;
        }
        cpus.sort_by(f64::total_cmp);
        let wall = walls.iter().copied().fold(f64::INFINITY, f64::min);
        let cpu = cpus[cpus.len() / 2];
        let gb = size as f64 / 1e9;
        let gib = size as f64 / (1u64 << 30) as f64;
        println!(
            "| {conns} | {:.2} | {:.3} | {:.0} | {:.3} |",
            gib / wall,
            cpu / gb,
            writes as f64 / gib,
            wall
        );
    }
}
