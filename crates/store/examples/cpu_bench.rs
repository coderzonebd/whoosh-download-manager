//! What the store costs in CPU per GiB on the engine's write path, split by
//! thread, with the network paced so the writer sees what it sees in a real
//! download (not a disk-bound flood).
//!
//! ```text
//! cargo run --release -p whoosh-store --example cpu_bench -- \
//!     [--dir DIR] [--size-mib 1024] [--conns 16] [--chunk-kib 64] \
//!     [--coalesce-kib 256] [--rate-mibs 800] [--flush-ms 2000] \
//!     [--rounds 3] [--store true|false] [--inline true|false]
//! ```
//!
//! `--conns` tasks each own an equal, 64 KiB-aligned share of a preallocated
//! file and receive it front to back as `--chunk-kib` body chunks (slices of
//! one buffer: the network's own copy is not part of this) at
//! `--rate-mibs` in total (0: as fast as the store takes them). Like the
//! engine's worker, each task gathers chunks into a `--coalesce-kib` buffer
//! (a copy) and writes it with one `write_at`; 0 writes every chunk as it
//! comes. A checkpoint task flushes every `--flush-ms`; the download ends
//! with `finish`. `--store false` runs the same without any storage call,
//! as the baseline; `--inline false` turns off
//! [`StorageOptions::write_inline`], so every write goes through the
//! writer thread.
//!
//! Reports per round: wall time, process CPU per GiB, page faults and
//! context switches per GiB, and per thread kind (the store's writer and
//! syncer threads, the runtime's workers) the CPU per GiB and the bytes it
//! wrote to the file (inline writes count for the runtime's workers),
//! sampled from `/proc/self/task` (Linux only).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use whoosh_core::StorageProvider;
use whoosh_store::{FileStorageProvider, StorageOptions};

#[derive(Debug, Clone)]
struct Args {
    dir: PathBuf,
    size: u64,
    conns: u64,
    chunk: usize,
    coalesce: usize,
    rate: u64,
    flush_every: Duration,
    rounds: usize,
    store: bool,
    inline: bool,
}

fn parse() -> Args {
    let mut a = Args {
        dir: PathBuf::from("."),
        size: 1 << 30,
        conns: 16,
        chunk: 64 << 10,
        coalesce: 256 << 10,
        rate: 800 << 20,
        flush_every: Duration::from_millis(2000),
        rounds: 3,
        store: true,
        inline: StorageOptions::default().write_inline,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dir" => a.dir = v.into(),
            "--size-mib" => a.size = v.parse::<u64>().unwrap() << 20,
            "--conns" => a.conns = v.parse().unwrap(),
            "--chunk-kib" => a.chunk = v.parse::<usize>().unwrap() << 10,
            "--coalesce-kib" => a.coalesce = v.parse::<usize>().unwrap() << 10,
            "--rate-mibs" => a.rate = v.parse::<u64>().unwrap() << 20,
            "--flush-ms" => a.flush_every = Duration::from_millis(v.parse().unwrap()),
            "--rounds" => a.rounds = v.parse().unwrap(),
            "--store" => a.store = v.parse().unwrap(),
            "--inline" => a.inline = v.parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

/// CPU seconds per thread name, keeping threads that have exited: sampled
/// every 5 ms until stopped.
struct Sampler {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<HashMap<String, (f64, u64)>>,
}

/// (tid, name, CPU ticks, bytes written by write syscalls).
fn thread_ticks() -> Vec<(String, String, u64, u64)> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return out;
    };
    for e in dir.flatten() {
        let Ok(s) = std::fs::read_to_string(e.path().join("stat")) else {
            continue;
        };
        let (Some(a), Some(b)) = (s.find('('), s.rfind(')')) else {
            continue;
        };
        let comm = s[a + 1..b].to_string();
        let f: Vec<&str> = s[b + 2..].split_whitespace().collect();
        let ticks = f[11].parse::<u64>().unwrap_or(0) + f[12].parse::<u64>().unwrap_or(0);
        let wchar = std::fs::read_to_string(e.path().join("io"))
            .ok()
            .and_then(|io| {
                io.lines()
                    .find_map(|l| l.strip_prefix("wchar: ").map(|v| v.trim().parse().ok()))
                    .flatten()
            })
            .unwrap_or(0);
        out.push((
            e.file_name().to_string_lossy().into_owned(),
            comm,
            ticks,
            wchar,
        ));
    }
    out
}

impl Sampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let base: HashMap<String, (u64, u64)> = thread_ticks()
            .into_iter()
            .map(|(tid, _, t, w)| (tid, (t, w)))
            .collect();
        let thread = std::thread::Builder::new()
            .name("sampler".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    let mut seen: HashMap<String, (String, u64, u64)> = HashMap::new();
                    loop {
                        let last = stop.load(Ordering::Relaxed);
                        for (tid, comm, t, w) in thread_ticks() {
                            // A new thread starts with its parent's name.
                            let e = seen.entry(tid).or_insert((String::new(), 0, 0));
                            e.0 = comm;
                            e.1 = e.1.max(t);
                            e.2 = e.2.max(w);
                        }
                        if last {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let tck = 100.0; // USER_HZ on Linux
                    let mut by: HashMap<String, (f64, u64)> = HashMap::new();
                    for (tid, (comm, t, w)) in seen {
                        let (t0, w0) = base.get(&tid).copied().unwrap_or((0, 0));
                        let e = by.entry(comm).or_default();
                        e.0 += t.saturating_sub(t0) as f64 / tck;
                        e.1 += w.saturating_sub(w0);
                    }
                    by
                }
            })
            .unwrap();
        Self { stop, thread }
    }

    fn stop(self) -> HashMap<String, (f64, u64)> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().unwrap()
    }
}

/// Process CPU time, minor page faults, context switches (voluntary +
/// involuntary).
#[cfg(unix)]
fn usage() -> (Duration, i64, i64) {
    use nix::sys::resource::{UsageWho, getrusage};
    let u = getrusage(UsageWho::RUSAGE_SELF).unwrap();
    let t =
        |tv: nix::sys::time::TimeVal| Duration::new(tv.tv_sec() as u64, tv.tv_usec() as u32 * 1000);
    (
        t(u.user_time()) + t(u.system_time()),
        u.minor_page_faults(),
        u.voluntary_context_switches() + u.involuntary_context_switches(),
    )
}

#[cfg(not(unix))]
fn usage() -> (Duration, i64, i64) {
    (Duration::ZERO, 0, 0)
}

struct Round {
    wall: Duration,
    cpu: Duration,
    faults: i64,
    switches: i64,
    /// Per thread name: CPU seconds, bytes written.
    threads: HashMap<String, (f64, u64)>,
}

async fn round(a: &Args, dir: &std::path::Path, src: Bytes) -> Round {
    let p = FileStorageProvider::new(StorageOptions {
        write_inline: a.inline,
        ..StorageOptions::default()
    });
    let part = dir.join("f.whoosh-part");
    let fin = dir.join("f");
    let sampler = Sampler::start();
    let (c0, f0, s0) = usage();
    let t0 = Instant::now();
    let s = if a.store {
        let s = p.open(&part).await.unwrap();
        s.preallocate(a.size).await.unwrap();
        Some(s)
    } else {
        None
    };
    let done = Arc::new(AtomicBool::new(false));
    let checkpoints = tokio::spawn({
        let (s, done, every) = (s.clone(), done.clone(), a.flush_every);
        async move {
            if every.is_zero() {
                return;
            }
            let mut next = Instant::now() + every;
            while !done.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if Instant::now() < next {
                    continue;
                }
                if let Some(s) = &s {
                    s.flush().await.unwrap();
                }
                next = Instant::now() + every;
            }
        }
    });
    // Equal 64 KiB-aligned shares, like the engine's split points.
    let share = (a.size / a.conns).div_ceil(64 << 10) * (64 << 10);
    let per_conn_rate = a.rate / a.conns;
    let tasks: Vec<_> = (0..a.conns)
        .map(|i| {
            let (s, src) = (s.clone(), src.clone());
            let (chunk, coalesce) = (a.chunk, a.coalesce);
            let start = (i * share).min(a.size);
            let end = ((i + 1) * share).min(a.size);
            tokio::spawn(async move {
                let t0 = Instant::now();
                let mut at = start;
                let mut gather = BytesMut::new();
                let mut gather_at = start;
                while at < end {
                    let n = (chunk as u64).min(end - at) as usize;
                    // The network: pace this connection to its share of the rate.
                    if per_conn_rate > 0 {
                        let due = Duration::from_secs_f64(
                            (at - start + n as u64) as f64 / per_conn_rate as f64,
                        );
                        if let Some(wait) = due.checked_sub(t0.elapsed()) {
                            tokio::time::sleep(wait).await;
                        }
                    }
                    let body = src.slice(..n);
                    at += n as u64;
                    if coalesce == 0 {
                        if let Some(s) = &s {
                            s.write_at(at - n as u64, body).await.unwrap();
                        }
                        continue;
                    }
                    if gather.is_empty() {
                        gather.reserve(coalesce);
                        gather_at = at - n as u64;
                    }
                    gather.extend_from_slice(&body);
                    if gather.len() >= coalesce || at == end {
                        let data = gather.split().freeze();
                        if let Some(s) = &s {
                            s.write_at(gather_at, data).await.unwrap();
                        }
                    }
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    done.store(true, Ordering::Relaxed);
    checkpoints.await.unwrap();
    if let Some(s) = s {
        s.finish(&fin).await.unwrap();
    }
    let wall = t0.elapsed();
    let (c1, f1, s1) = usage();
    let mut threads = sampler.stop();
    // Not part of what is measured.
    let sampled = threads.remove("sampler").map_or(0.0, |(c, _)| c);
    let _ = std::fs::remove_file(&fin);
    Round {
        wall,
        cpu: (c1 - c0).saturating_sub(Duration::from_secs_f64(sampled)),
        faults: f1 - f0,
        switches: s1 - s0,
        threads,
    }
}

fn main() {
    let a = parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let src: Bytes = (0..a.chunk.max(1))
        .map(|i| (i * 31 % 251) as u8)
        .collect::<Vec<_>>()
        .into();
    let tmp = tempfile::Builder::new()
        .prefix("whoosh-cpu-bench-")
        .tempdir_in(&a.dir)
        .unwrap();
    println!(
        "cpu_bench: {} MiB, {} conns, {} KiB chunks, gather {} KiB, {} MiB/s, flush every {} ms, store {}, inline {}, dir {}",
        a.size >> 20,
        a.conns,
        a.chunk >> 10,
        a.coalesce >> 10,
        a.rate >> 20,
        a.flush_every.as_millis(),
        a.store,
        a.inline,
        std::fs::canonicalize(tmp.path()).unwrap().display()
    );
    let gib = a.size as f64 / (1u64 << 30) as f64;
    for _ in 0..a.rounds {
        let r = rt.block_on(round(&a, tmp.path(), src.clone()));
        let mut kinds: Vec<_> = r
            .threads
            .into_iter()
            .filter(|(_, (c, w))| *c > 0.0 || *w > 0)
            .collect();
        kinds.sort_by(|x, y| x.0.cmp(&y.0));
        let split: Vec<String> = kinds
            .iter()
            .map(|(k, (c, w))| {
                if *w >= 1 << 20 {
                    format!("{k} {:.3} ({} MiB written)", c / gib, w >> 20)
                } else {
                    format!("{k} {:.3}", c / gib)
                }
            })
            .collect();
        println!(
            "wall {:7.1} ms  cpu {:.3} s/GiB  faults {:.0}/GiB  switches {:.0}/GiB  [{}]",
            r.wall.as_secs_f64() * 1e3,
            r.cpu.as_secs_f64() / gib,
            r.faults as f64 / gib,
            r.switches as f64 / gib,
            split.join(", ")
        );
    }
}
