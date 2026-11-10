//! CPU cost of the engine's data path: push 1 GiB through the engine
//! in-process with 1, 4 and 16 connections, and report throughput and CPU
//! seconds per GB.
//!
//! The source serves zero-copy slices of a repeating pattern and the
//! storage discards writes (counting them), so the numbers are the
//! engine's own cost: scheduling, claims, counters, the per-write
//! `BoxFuture`, task wake-ups. Network and disk come on top.
//!
//! ```text
//! cargo run --release -p vayu-engine --example throughput -- [GiB] [chunk KiB] [runs]
//! ```

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::future::BoxFuture;
use vayu_core::testing::{MemJournal, MemSourceFactory, MemStateStore};
use vayu_core::{
    ByteStream, OpenRequest, Protocol, RangeSource, RemoteMeta, SourceError, Storage,
    StorageProvider,
};
use vayu_engine::{Engine, EngineConfig, EngineDeps, Tuning};
use vayu_proto::service::Service;
use vayu_proto::{AddParams, DownloadState, IdParams};

const PATTERN: usize = 4 << 20;

struct PatternSource {
    size: u64,
    chunk: usize,
    pattern: Bytes,
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
        let s = futures::stream::unfold(start, move |pos| {
            let pattern = pattern.clone();
            async move {
                if pos >= end {
                    return None;
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

#[derive(Default)]
struct NullStorage {
    written: AtomicU64,
}

impl Storage for NullStorage {
    fn preallocate(&self, _len: u64) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn write_at(&self, _offset: u64, data: Bytes) -> BoxFuture<'_, io::Result<()>> {
        self.written.fetch_add(data.len() as u64, Ordering::Relaxed);
        Box::pin(async { Ok(()) })
    }
    fn read_at(&self, _offset: u64, len: usize) -> BoxFuture<'_, io::Result<Bytes>> {
        Box::pin(async move { Ok(Bytes::from(vec![0; len])) })
    }
    fn flush(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn finish<'a>(&'a self, _p: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Default)]
struct NullProvider {
    last: std::sync::Mutex<Option<Arc<NullStorage>>>,
}

impl StorageProvider for NullProvider {
    fn open<'a>(&'a self, _p: &'a Path) -> BoxFuture<'a, io::Result<Arc<dyn Storage>>> {
        let s = Arc::new(NullStorage::default());
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

async fn one(size: u64, chunk: usize, conns: u16) -> (f64, f64) {
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
        }),
    );
    let storage = Arc::new(NullProvider::default());
    let mut cfg = EngineConfig::new("/bench");
    cfg.tuning = Tuning::default();
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
    let written = storage
        .last
        .lock()
        .unwrap()
        .as_ref()
        .map_or(0, |s| s.written.load(Ordering::Relaxed));
    assert_eq!(written, size, "every byte written exactly once");
    engine.shutdown().await;
    (wall, cpu)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gib: f64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(1.0);
    let chunk_kib: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(64);
    let runs: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(3);
    let size = (gib * (1u64 << 30) as f64) as u64;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    println!(
        "engine data path, {gib} GiB, {chunk_kib} KiB chunks, best of {runs} (null source/storage, 4 worker threads)"
    );
    println!("| connections | GiB/s | CPU s per GB | wall s |");
    println!("|---:|---:|---:|---:|");
    for conns in [1u16, 4, 16] {
        let mut best: Option<(f64, f64)> = None;
        for _ in 0..runs {
            let (wall, cpu) = rt.block_on(one(size, chunk_kib * 1024, conns));
            if best.is_none_or(|b| wall < b.0) {
                best = Some((wall, cpu));
            }
        }
        let (wall, cpu) = best.unwrap();
        let gb = size as f64 / 1e9;
        println!(
            "| {conns} | {:.2} | {:.3} | {:.3} |",
            size as f64 / (1u64 << 30) as f64 / wall,
            cpu / gb,
            wall
        );
    }
}
