//! A download as the engine writes it, with checkpoints: what a `flush`
//! costs the writers.
//!
//! ```text
//! cargo run --release -p vayu-store --example download_bench -- \
//!     [--dir DIR] [--size-gib 1] [--segments 8] [--chunk-kib 256] \
//!     [--flush-mib 64] [--flush-ms 0] [--buffer-mib 16] [--runs 3] \
//!     [--drop-cache true|false]
//! ```
//!
//! `--segments` tasks each write their share of a preallocated file front to
//! back in `--chunk-kib` chunks (the engine's coalesced write size), as fast
//! as the store accepts them: the network is never the limit, so this is the
//! disk-bound case. A checkpoint task calls `flush` every `--flush-mib` MiB
//! accepted, or every `--flush-ms` ms, while the writers keep going; a last
//! `flush` ends the run, as the engine's `finish` does.
//!
//! Reports the total time and rate, every checkpoint flush's latency
//! (n, min, p50, p90, max, sum), the final flush, how long the slowest
//! single `write_at` waited for buffer space, and CPU time.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use vayu_core::StorageProvider;
use vayu_store::{FileStorageProvider, StorageOptions};

#[derive(Debug, Clone)]
struct Args {
    dir: PathBuf,
    size: u64,
    segments: u64,
    chunk: usize,
    flush_bytes: u64,
    flush_every: Duration,
    buffer: usize,
    drop_cache: bool,
    runs: usize,
}

fn parse() -> Args {
    let mut a = Args {
        dir: PathBuf::from("."),
        size: 1 << 30,
        segments: 8,
        chunk: 256 << 10,
        flush_bytes: 64 << 20,
        flush_every: Duration::ZERO,
        buffer: 16 << 20,
        drop_cache: StorageOptions::default().drop_cache_after_flush,
        runs: 3,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dir" => a.dir = v.into(),
            "--size-gib" => a.size = (v.parse::<f64>().unwrap() * (1u64 << 30) as f64) as u64,
            "--segments" => a.segments = v.parse().unwrap(),
            "--chunk-kib" => a.chunk = v.parse::<usize>().unwrap() << 10,
            "--flush-mib" => a.flush_bytes = v.parse::<u64>().unwrap() << 20,
            "--flush-ms" => a.flush_every = Duration::from_millis(v.parse().unwrap()),
            "--buffer-mib" => a.buffer = v.parse::<usize>().unwrap() << 20,
            "--runs" => a.runs = v.parse().unwrap(),
            "--drop-cache" => a.drop_cache = v.parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

#[cfg(unix)]
fn cpu() -> Duration {
    use nix::sys::resource::{UsageWho, getrusage};
    let u = getrusage(UsageWho::RUSAGE_SELF).unwrap();
    let t =
        |tv: nix::sys::time::TimeVal| Duration::new(tv.tv_sec() as u64, tv.tv_usec() as u32 * 1000);
    t(u.user_time()) + t(u.system_time())
}

#[cfg(not(unix))]
fn cpu() -> Duration {
    Duration::ZERO
}

struct Run {
    total: Duration,
    flushes: Vec<Duration>,
    final_flush: Duration,
    worst_write: Duration,
    cpu: Duration,
}

async fn run(a: &Args, path: &std::path::Path, data: Bytes) -> Run {
    let p = FileStorageProvider::new(StorageOptions {
        buffer_bytes: a.buffer,
        drop_cache_after_flush: a.drop_cache,
    });
    let s = p.open(path).await.unwrap();
    s.preallocate(a.size).await.unwrap();
    let accepted = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let worst = Arc::new(AtomicU64::new(0));
    let c0 = cpu();
    let t0 = Instant::now();

    let checkpoints = tokio::spawn({
        let (s, accepted, done) = (s.clone(), accepted.clone(), done.clone());
        let (flush_bytes, flush_every) = (a.flush_bytes, a.flush_every);
        async move {
            let mut lat = Vec::new();
            let mut next_bytes = flush_bytes;
            let mut next_time = Instant::now() + flush_every;
            while !done.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let due_bytes = flush_bytes > 0 && accepted.load(Ordering::Relaxed) >= next_bytes;
                let due_time = !flush_every.is_zero() && Instant::now() >= next_time;
                if !(due_bytes || due_time) {
                    continue;
                }
                let t = Instant::now();
                s.flush().await.unwrap();
                lat.push(t.elapsed());
                next_bytes = accepted.load(Ordering::Relaxed) + flush_bytes;
                next_time = Instant::now() + flush_every;
            }
            lat
        }
    });

    let seg = a.size / a.segments;
    let writers: Vec<_> = (0..a.segments)
        .map(|i| {
            let (s, data, accepted, worst) =
                (s.clone(), data.clone(), accepted.clone(), worst.clone());
            let chunk = a.chunk as u64;
            let end = if i + 1 == a.segments {
                a.size
            } else {
                (i + 1) * seg
            };
            tokio::spawn(async move {
                let mut at = i * seg;
                while at < end {
                    let n = chunk.min(end - at);
                    let t = Instant::now();
                    s.write_at(at, data.slice(..n as usize)).await.unwrap();
                    worst.fetch_max(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                    accepted.fetch_add(n, Ordering::Relaxed);
                    at += n;
                }
            })
        })
        .collect();
    for w in writers {
        w.await.unwrap();
    }
    done.store(true, Ordering::Relaxed);
    let flushes = checkpoints.await.unwrap();
    let t = Instant::now();
    s.flush().await.unwrap();
    let final_flush = t.elapsed();
    let total = t0.elapsed();
    let cpu = cpu() - c0;
    Run {
        total,
        flushes,
        final_flush,
        worst_write: Duration::from_micros(worst.load(Ordering::Relaxed)),
        cpu,
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn main() {
    let a = parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let data: Bytes = (0..a.chunk)
        .map(|i| (i * 31 % 251) as u8)
        .collect::<Vec<_>>()
        .into();
    println!(
        "download: {:.2} GiB, {} segments, {} KiB chunks, flush every {}, buffer {} MiB, drop cache {}, dir {}",
        a.size as f64 / (1u64 << 30) as f64,
        a.segments,
        a.chunk >> 10,
        if a.flush_every.is_zero() {
            format!("{} MiB", a.flush_bytes >> 20)
        } else {
            format!("{} ms", a.flush_every.as_millis())
        },
        a.buffer >> 20,
        a.drop_cache,
        std::fs::canonicalize(&a.dir).unwrap().display()
    );
    let mut all_flushes = Vec::new();
    for r in 1..=a.runs {
        let tmp = tempfile::Builder::new()
            .prefix("vayu-download-bench-")
            .tempdir_in(&a.dir)
            .unwrap();
        let res = rt.block_on(run(&a, &tmp.path().join("bench.vayu-part"), data.clone()));
        let mut f = res.flushes.clone();
        f.sort();
        let pick = |q: f64| {
            f.get(((f.len() as f64 - 1.0) * q).round() as usize)
                .copied()
        };
        let fmt = |d: Option<Duration>| d.map_or("-".into(), |d| format!("{:.0}", ms(d)));
        println!(
            "run {r}: total {:.2} s = {:.2} GB/s | {} flushes, ms: min {} p50 {} p90 {} max {} sum {:.0} | final flush {:.0} ms | worst write_at wait {:.0} ms | cpu {:.2} s",
            res.total.as_secs_f64(),
            a.size as f64 / 1e9 / res.total.as_secs_f64(),
            f.len(),
            fmt(f.first().copied()),
            fmt(pick(0.5)),
            fmt(pick(0.9)),
            fmt(f.last().copied()),
            ms(f.iter().sum()),
            ms(res.final_flush),
            ms(res.worst_write),
            res.cpu.as_secs_f64(),
        );
        all_flushes.extend(f);
        drop(tmp);
    }
    all_flushes.sort();
    if !all_flushes.is_empty() {
        let n = all_flushes.len();
        println!(
            "all runs: {n} checkpoint flushes, ms: p10 {:.0} p50 {:.0} p90 {:.0} max {:.0}",
            ms(all_flushes[n / 10]),
            ms(all_flushes[n / 2]),
            ms(all_flushes[(n * 9) / 10]),
            ms(all_flushes[n - 1])
        );
    }
}
