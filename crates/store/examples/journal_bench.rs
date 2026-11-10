//! Journal commit latency, with and without fsync.
//!
//! ```text
//! cargo run --release -p vayu-store --example journal_bench -- [--dir DIR] [--commits 1000] [--ranges 16]
//! ```
//!
//! Commits a typical checkpoint (`--ranges` done ranges, ETag, final URL)
//! to one id in a loop, then to 16 ids in parallel (16 downloads at once).
//! The no-fsync figures show what the format and the I/O path cost; only
//! the fsync figures are what the engine sees.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vayu_core::source::RemoteMeta;
use vayu_core::{ByteRange, Checkpoint, Journal, RangeSet};
use vayu_store::FileJournal;

fn checkpoint(n: u64, ranges: u64) -> Checkpoint {
    Checkpoint {
        meta: RemoteMeta {
            size: Some(4 << 30),
            accepts_ranges: true,
            etag: Some("\"5f3a9c2e-100000000\"".into()),
            last_modified: Some("Mon, 10 Nov 2026 08:00:00 GMT".into()),
            final_url: Some(
                "https://cdn.example.com/releases/v1.2.3/big-file.iso?sig=0123456789abcdef".into(),
            ),
            ..RemoteMeta::default()
        },
        done: RangeSet::from_ranges((0..ranges).map(|i| {
            let start = i * (256 << 20);
            ByteRange::new(start, start + (n * 65_536) % (200 << 20) + 1).unwrap()
        })),
    }
}

fn stats(name: &str, mut v: Vec<Duration>) {
    v.sort();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let mean = v.iter().sum::<Duration>() / v.len() as u32;
    println!(
        "{name:<34} n={:<5} mean {:>8.1} µs  p50 {:>8.1} µs  p99 {:>8.1} µs  max {:>8.1} µs",
        v.len(),
        us(mean),
        us(v[v.len() / 2]),
        us(v[v.len() * 99 / 100]),
        us(v[v.len() - 1]),
    );
}

async fn sequential(j: &FileJournal, commits: u64, ranges: u64) -> Vec<Duration> {
    let id = "bench-seq".to_string();
    let mut out = Vec::new();
    for n in 0..commits {
        let cp = checkpoint(n, ranges);
        let t = Instant::now();
        j.commit(&id, &cp).await.unwrap();
        out.push(t.elapsed());
    }
    out
}

async fn parallel(j: Arc<FileJournal>, commits: u64, ranges: u64) -> (Vec<Duration>, Duration) {
    let t0 = Instant::now();
    let tasks: Vec<_> = (0..16)
        .map(|k| {
            let j = j.clone();
            tokio::spawn(async move {
                let id = format!("bench-par-{k}");
                let mut out = Vec::new();
                for n in 0..commits / 16 {
                    let cp = checkpoint(n, ranges);
                    let t = Instant::now();
                    j.commit(&id, &cp).await.unwrap();
                    out.push(t.elapsed());
                }
                out
            })
        })
        .collect();
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    (all, t0.elapsed())
}

fn main() {
    let mut dir = PathBuf::from(".");
    let (mut commits, mut ranges) = (1000u64, 16u64);
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dir" => dir = v.into(),
            "--commits" => commits = v.parse().unwrap(),
            "--ranges" => ranges = v.parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let payload = serde_json::to_vec(&checkpoint(1, ranges)).unwrap().len();
    println!(
        "checkpoint payload {payload} bytes ({ranges} ranges), dir {}",
        std::fs::canonicalize(&dir).unwrap().display()
    );
    rt.block_on(async {
        for sync in [false, true] {
            let tmp = tempfile::Builder::new()
                .prefix("vayu-journal-bench-")
                .tempdir_in(&dir)
                .unwrap();
            let mut j = FileJournal::open(tmp.path()).unwrap();
            if !sync {
                j = j.danger_disable_fsync_for_benchmarks();
            }
            let label = if sync { "fsync" } else { "no fsync" };
            stats(
                &format!("commit, 1 download ({label})"),
                sequential(&j, commits, ranges).await,
            );
            let (lat, wall) = parallel(Arc::new(j), commits, ranges).await;
            let n = lat.len();
            stats(&format!("commit, 16 downloads ({label})"), lat);
            println!(
                "{:<34} {:.0} commits/s overall",
                "",
                n as f64 / wall.as_secs_f64()
            );
        }
    });
}
