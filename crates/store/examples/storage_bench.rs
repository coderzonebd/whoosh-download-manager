//! Storage throughput: concurrent writers put fixed-size chunks at random
//! offsets into a preallocated file, then one `flush` makes it all durable.
//!
//! ```text
//! cargo run --release -p vayu-store --example storage_bench -- \
//!     [--dir DIR] [--size-gib 4] [--writers 16] [--chunk-kib 1024] \
//!     [--buffer-mib 16] [--mode store|raw] [--runs 1]
//! ```
//!
//! `--dir` must be on the disk you want to measure (default: the current
//! directory; a temporary directory is created inside it and removed).
//! `--mode raw` is the baseline: the same writes as plain `pwrite` calls
//! from as many OS threads, no buffering, no coalescing. With
//! `BENCH_THREADS=1` (Linux) it also prints CPU time per thread name.
//!
//! Reports preallocation time, then wall time, GB/s (10^9 bytes) and CPU
//! time (user and system, all threads) for accepting every write and for
//! accepting plus the final flush. Preallocation is timed on its own (on
//! tmpfs it zeroes real memory and would swamp the write figures).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bytes::Bytes;
use vayu_core::StorageProvider;
use vayu_store::{FileStorageProvider, StorageOptions};

#[derive(Debug, Clone)]
struct Args {
    dir: PathBuf,
    size: u64,
    writers: usize,
    chunk: usize,
    buffer: usize,
    raw: bool,
    runs: usize,
}

fn parse() -> Args {
    let mut a = Args {
        dir: PathBuf::from("."),
        size: 4 << 30,
        writers: 16,
        chunk: 1 << 20,
        buffer: 16 << 20,
        raw: false,
        runs: 1,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--dir" => a.dir = v().into(),
            "--size-gib" => a.size = (v().parse::<f64>().unwrap() * (1u64 << 30) as f64) as u64,
            "--writers" => a.writers = v().parse().unwrap(),
            "--chunk-kib" => a.chunk = v().parse::<usize>().unwrap() << 10,
            "--buffer-mib" => a.buffer = v().parse::<usize>().unwrap() << 20,
            "--mode" => a.raw = v() == "raw",
            "--runs" => a.runs = v().parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

/// CPU time of the whole process so far (all threads): (user, system).
#[derive(Debug, Clone, Copy, Default)]
struct Cpu {
    user: Duration,
    sys: Duration,
}

impl std::ops::Sub for Cpu {
    type Output = Cpu;
    fn sub(self, o: Cpu) -> Cpu {
        Cpu {
            user: self.user - o.user,
            sys: self.sys - o.sys,
        }
    }
}

impl Cpu {
    fn total(&self) -> Duration {
        self.user + self.sys
    }
}

#[cfg(unix)]
fn cpu() -> Cpu {
    use nix::sys::resource::{UsageWho, getrusage};
    let u = getrusage(UsageWho::RUSAGE_SELF).unwrap();
    let t =
        |tv: nix::sys::time::TimeVal| Duration::new(tv.tv_sec() as u64, tv.tv_usec() as u32 * 1000);
    Cpu {
        user: t(u.user_time()),
        sys: t(u.system_time()),
    }
}

#[cfg(not(unix))]
fn cpu() -> Cpu {
    Cpu::default()
}

#[cfg(not(unix))]
fn fs_name(_dir: &std::path::Path) -> String {
    "unknown".into()
}

#[cfg(unix)]
fn fs_name(dir: &std::path::Path) -> String {
    let Ok(s) = rustix::fs::statfs(dir) else {
        return "unknown".into();
    };
    match s.f_type as u64 {
        0xEF53 => "ext4".into(),
        0x5846_5342 => "xfs".into(),
        0x9123_683E => "btrfs".into(),
        0x0102_1994 => "tmpfs".into(),
        0x2FC1_2FC1 => "zfs".into(),
        0x6573_5546 => "fuse".into(),
        0x4d44 => "vfat".into(),
        0x2011_BAB0 => "exfat".into(),
        0x5346_544e => "ntfs".into(),
        0x6969 => "nfs".into(),
        t => format!("0x{t:x}"),
    }
}

/// Chunk offsets in a shuffled order, dealt round-robin to the writers.
fn plan(a: &Args) -> Vec<Vec<u64>> {
    let n = a.size / a.chunk as u64;
    let mut offsets: Vec<u64> = (0..n).map(|i| i * a.chunk as u64).collect();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for i in (1..offsets.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        offsets.swap(i, (seed % (i as u64 + 1)) as usize);
    }
    let mut per = vec![Vec::new(); a.writers];
    for (i, o) in offsets.into_iter().enumerate() {
        per[i % a.writers].push(o);
    }
    per
}

struct Times {
    prealloc: Duration,
    accept: Duration,
    flush: Duration,
    /// Accepting every write (preallocation excluded).
    cpu_accept: Cpu,
    /// Accepting plus the final flush (preallocation excluded).
    cpu_writes: Cpu,
}

async fn run_store(a: &Args, path: &std::path::Path, data: Bytes) -> Times {
    let p = FileStorageProvider::new(StorageOptions {
        buffer_bytes: a.buffer,
    });
    let s = p.open(path).await.unwrap();
    let t0 = Instant::now();
    s.preallocate(a.size).await.unwrap();
    let prealloc = t0.elapsed();
    let c0 = cpu();
    let t1 = Instant::now();
    let tasks: Vec<_> = plan(a)
        .into_iter()
        .map(|offsets| {
            let (s, data) = (s.clone(), data.clone());
            tokio::spawn(async move {
                for o in offsets {
                    s.write_at(o, data.clone()).await.unwrap();
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    let accept = t1.elapsed();
    let c1 = cpu();
    let t2 = Instant::now();
    s.flush().await.unwrap();
    let flush = t2.elapsed();
    let c2 = cpu();
    print_threads();
    Times {
        prealloc,
        accept,
        flush,
        cpu_accept: c1 - c0,
        cpu_writes: c2 - c0,
    }
}

#[cfg(not(unix))]
fn run_raw(_a: &Args, _path: &std::path::Path, _data: Bytes) -> Times {
    panic!("--mode raw is Unix-only")
}

#[cfg(unix)]
fn run_raw(a: &Args, path: &std::path::Path, data: Bytes) -> Times {
    use std::os::unix::fs::FileExt;
    let f = std::sync::Arc::new(
        std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap(),
    );
    let t0 = Instant::now();
    rustix::fs::fallocate(&*f, rustix::fs::FallocateFlags::empty(), 0, a.size).unwrap();
    let prealloc = t0.elapsed();
    let c0 = cpu();
    let t1 = Instant::now();
    let threads: Vec<_> = plan(a)
        .into_iter()
        .map(|offsets| {
            let (f, data) = (f.clone(), data.clone());
            std::thread::spawn(move || {
                for o in offsets {
                    f.write_all_at(&data, o).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let accept = t1.elapsed();
    let c1 = cpu();
    let t2 = Instant::now();
    f.sync_data().unwrap();
    let flush = t2.elapsed();
    let c2 = cpu();
    print_threads();
    Times {
        prealloc,
        accept,
        flush,
        cpu_accept: c1 - c0,
        cpu_writes: c2 - c0,
    }
}

/// CPU (user + system) per thread name so far, from /proc (Linux): shows
/// whether time goes to the writer thread (the kernel write path) or to the
/// runtime's workers (wakeups, futexes).
#[cfg(target_os = "linux")]
fn print_threads() {
    if std::env::var_os("BENCH_THREADS").is_none() {
        return;
    }
    // /proc reports in USER_HZ, which is 100 on every Linux ABI.
    let tick = 100.0;
    let mut by_name = std::collections::BTreeMap::<String, (f64, f64)>::new();
    for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let Ok(stat) = std::fs::read_to_string(t.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].to_string();
        let f: Vec<&str> = stat[close + 2..].split(' ').collect();
        let (u, s) = (f[11].parse::<f64>().unwrap(), f[12].parse::<f64>().unwrap());
        let e = by_name.entry(name).or_default();
        e.0 += u / tick;
        e.1 += s / tick;
    }
    for (name, (u, s)) in by_name {
        if u + s > 0.005 {
            println!("    thread {name:<18} user {u:.2} s  sys {s:.2} s");
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn print_threads() {}

fn main() {
    let a = parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let data: Bytes = {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        (0..a.chunk)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect::<Vec<_>>()
            .into()
    };
    let gb = a.size as f64 / 1e9;
    println!(
        "mode={} size={:.2} GiB writers={} chunk={} KiB buffer={} MiB fs={} dir={}",
        if a.raw { "raw-pwrite" } else { "vayu-store" },
        a.size as f64 / (1u64 << 30) as f64,
        a.writers,
        a.chunk >> 10,
        a.buffer >> 20,
        fs_name(&a.dir),
        std::fs::canonicalize(&a.dir).unwrap().display()
    );
    for run in 1..=a.runs {
        let tmp = tempfile::Builder::new()
            .prefix("vayu-storage-bench-")
            .tempdir_in(&a.dir)
            .unwrap();
        let path = tmp.path().join("bench.vayu-part");
        let t = if a.raw {
            run_raw(&a, &path, data.clone())
        } else {
            rt.block_on(run_store(&a, &path, data.clone()))
        };
        let writes = t.accept + t.flush;
        let pct = |c: Duration, w: Duration| 100.0 * c.as_secs_f64() / w.as_secs_f64();
        println!(
            "run {run}: prealloc {:.0} ms | accept {:.2} s = {:.2} GB/s, cpu {:.2} s ({:.0}% of a core) | flush {:.2} s | writes+flush {:.2} s = {:.2} GB/s, cpu {:.2} s = user {:.2} + sys {:.2} ({:.0}% of a core, {:.2} cpu-s/GB)",
            t.prealloc.as_secs_f64() * 1e3,
            t.accept.as_secs_f64(),
            gb / t.accept.as_secs_f64(),
            t.cpu_accept.total().as_secs_f64(),
            pct(t.cpu_accept.total(), t.accept),
            t.flush.as_secs_f64(),
            writes.as_secs_f64(),
            gb / writes.as_secs_f64(),
            t.cpu_writes.total().as_secs_f64(),
            t.cpu_writes.user.as_secs_f64(),
            t.cpu_writes.sys.as_secs_f64(),
            pct(t.cpu_writes.total(), writes),
            t.cpu_writes.total().as_secs_f64() / gb,
        );
        drop(tmp);
    }
}
