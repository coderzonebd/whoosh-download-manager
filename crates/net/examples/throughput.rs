//! Throughput and CPU cost of `vayu-net` against labserver's `clean`
//! scenario, over HTTP/1.1, HTTP/2 and HTTP/3, with 1, 4 and 16 concurrent
//! opens.
//!
//! ```text
//! cargo run --release -p vayu-net --example throughput -- \
//!     [--transports h1,h2,h3] [--conns 1,4,16] [--file 100m.bin] \
//!     [--gb 1] [--runs 5] [--threads 4] [--cache-mb 256] [--compare]
//! ```
//!
//! The server runs in a child process (this binary with `--serve`), so the
//! CPU time measured here (`getrusage(RUSAGE_SELF)`) is the client's
//! alone. Each run moves `--gb` gigabytes: N workers each fetch their Nth of
//! the file repeatedly (one ranged request per pass, as the engine does),
//! on connections opened by an unmeasured warm-up pass. `--compare` also
//! times `wget` and `curl` fetching the same bytes over one HTTP/1.1
//! connection (their CPU from `RUSAGE_CHILDREN`).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use vayu_core::{ByteRange, OpenRequest, Protocol, RangeSource, SourceFactory};
use vayu_net::{HttpSourceFactory, NetConfig};

#[derive(Debug, Clone)]
struct Args {
    serve: bool,
    transports: Vec<String>,
    conns: Vec<u64>,
    file: String,
    gb: f64,
    runs: usize,
    threads: usize,
    cache_mb: usize,
    compare: bool,
    /// Use a server started separately with `--serve` (its READY line).
    attach: Option<String>,
}

fn parse() -> Args {
    let mut a = Args {
        serve: false,
        transports: vec!["h1".into(), "h2".into(), "h3".into()],
        conns: vec![1, 4, 16],
        file: "100m.bin".into(),
        gb: 1.0,
        runs: 5,
        threads: 4,
        cache_mb: 256,
        compare: false,
        attach: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--serve" => a.serve = true,
            "--compare" => a.compare = true,
            "--transports" => a.transports = val().split(',').map(Into::into).collect(),
            "--conns" => a.conns = val().split(',').map(|n| n.parse().unwrap()).collect(),
            "--file" => a.file = val(),
            "--gb" => a.gb = val().parse().unwrap(),
            "--runs" => a.runs = val().parse().unwrap(),
            "--threads" => a.threads = val().parse().unwrap(),
            "--cache-mb" => a.cache_mb = val().parse().unwrap(),
            "--attach" => a.attach = Some(val()),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

/// `--serve`: labserver (built-in scenarios, HTTP/3 on) until stdin closes.
fn serve(a: &Args) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let lab = vayu_labserver::LabServer::start(vayu_labserver::LabConfig {
            h3_port: Some(0),
            cache_bytes: a.cache_mb << 20,
            ..Default::default()
        })
        .await
        .unwrap();
        let cert: String = lab
            .cert_der()
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!(
            "READY {} {} {} {cert}",
            lab.http_addr(),
            lab.https_addr().unwrap(),
            lab.h3_addr().unwrap()
        );
        std::io::stdout().flush().unwrap();
        tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            while std::io::stdin().read_line(&mut s).is_ok_and(|n| n > 0) {}
        })
        .await
        .unwrap();
    });
}

struct Server {
    http: String,
    https: String,
    h3_port: u16,
    cert: Vec<u8>,
}

/// CPU seconds (user + system) of this process, or of finished children.
#[cfg(unix)]
fn cpu(children: bool) -> f64 {
    use nix::sys::resource::{UsageWho, getrusage};
    let who = if children {
        UsageWho::RUSAGE_CHILDREN
    } else {
        UsageWho::RUSAGE_SELF
    };
    let u = getrusage(who).unwrap();
    let s = |t: nix::sys::time::TimeVal| t.tv_sec() as f64 + t.tv_usec() as f64 / 1e6;
    s(u.user_time()) + s(u.system_time())
}

#[cfg(not(unix))]
fn cpu(_children: bool) -> f64 {
    f64::NAN
}

fn loadavg() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|_| "?".into())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// N workers, each fetching its slice of the file `reps` times. Returns
/// bytes and chunks received.
async fn pass(
    src: &Arc<dyn RangeSource>,
    size: u64,
    n: u64,
    reps: u64,
    expect: &vayu_core::Validators,
) -> (u64, u64) {
    let len = size / n;
    let tasks: Vec<_> = (0..n)
        .map(|i| {
            let src = src.clone();
            let expect = expect.clone();
            let end = if i == n - 1 { size } else { (i + 1) * len };
            let range = ByteRange::new(i * len, end).unwrap();
            tokio::spawn(async move {
                let (mut got, mut chunks) = (0u64, 0u64);
                for _ in 0..reps {
                    let mut s = src
                        .open(OpenRequest {
                            range: Some(range),
                            expect: expect.clone(),
                        })
                        .await
                        .expect("open");
                    while let Some(c) = s.next().await {
                        got += c.expect("read").len() as u64;
                        chunks += 1;
                    }
                }
                (got, chunks)
            })
        })
        .collect();
    let (mut total, mut chunks) = (0, 0);
    for t in tasks {
        let (b, c) = t.await.unwrap();
        total += b;
        chunks += c;
    }
    (total, chunks)
}

async fn run(a: &Args, srv: &Server, transport: &str, n: u64) {
    let cfg = NetConfig {
        extra_root_certs: vec![srv.cert.clone()],
        platform_roots: false,
        h3_fallback_delay: Duration::from_secs(5),
        // The server advertises HTTP/3; only the h3 rows may follow it.
        http3: transport == "h3",
        ..NetConfig::default()
    };
    let factory = HttpSourceFactory::new(cfg);
    let (url, want) = match transport {
        "h1" => (
            format!("http://{}/s/clean/{}", srv.http, a.file),
            Protocol::Http1,
        ),
        "h2" => (
            format!("https://{}/s/clean/{}", srv.https, a.file),
            Protocol::Http2,
        ),
        "h3" => {
            let origin = format!("https://{}/", srv.https);
            factory.remember_http3(&origin, srv.h3_port, Duration::from_secs(3600));
            (
                format!("https://{}/s/clean/{}", srv.https, a.file),
                Protocol::Http3,
            )
        }
        t => panic!("unknown transport {t}"),
    };
    let src = factory.source(&url, None).unwrap();
    let meta = src.probe().await.unwrap();
    assert_eq!(meta.protocol, Some(want), "{transport}");
    let size = meta.size.unwrap();
    let expect = meta.validators();
    let reps = ((a.gb * 1e9) / size as f64).ceil().max(1.0) as u64;
    let _ = pass(&src, size, n, 1, &expect).await; // warm-up: connections, caches
    let load = loadavg();
    let (mut rates, mut cpus, mut chunk) = (Vec::new(), Vec::new(), 0.0);
    for _ in 0..a.runs {
        let (c0, t0) = (cpu(false), Instant::now());
        let (bytes, chunks) = pass(&src, size, n, reps, &expect).await;
        chunk = bytes as f64 / chunks as f64 / 1024.0;
        let (dt, dc) = (t0.elapsed().as_secs_f64(), cpu(false) - c0);
        assert_eq!(bytes, size * reps);
        let _ = chunks;
        rates.push(bytes as f64 / dt / 1e9);
        cpus.push(dc / (bytes as f64 / 1e9));
    }
    let best = rates.iter().cloned().fold(0.0, f64::max);
    println!(
        "| {transport} | {n:>2} | {:>5.2} | {best:>5.2} | {:>5.2} | {chunk:>5.0} | {load} |",
        median(rates),
        median(cpus)
    );
}

fn compare(a: &Args, srv: &Server) {
    let url = format!("http://{}/s/clean/{}", srv.http, a.file);
    let probe = Command::new("curl").args(["-sI", &url]).output();
    let size: f64 = probe
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout).lines().find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().to_string())
            })
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(100.0 * 1048576.0);
    let reps = ((a.gb * 1e9) / size).ceil().max(1.0) as usize;
    let urls = vec![url.as_str(); reps];
    for (tool, args) in [
        ("wget", vec!["-q", "-O", "/dev/null"]),
        ("curl", vec!["-s"]),
    ] {
        let (mut rates, mut cpus) = (Vec::new(), Vec::new());
        for _ in 0..a.runs {
            let (c0, t0) = (cpu(true), Instant::now());
            let ok = Command::new(tool)
                .args(&args)
                .args(&urls)
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            let (dt, dc) = (t0.elapsed().as_secs_f64(), cpu(true) - c0);
            if !ok {
                println!("| {tool} | failed | | | | | |");
                break;
            }
            let gb = size * reps as f64 / 1e9;
            rates.push(gb / dt);
            cpus.push(dc / gb);
        }
        if !rates.is_empty() {
            let best = rates.iter().cloned().fold(0.0, f64::max);
            println!(
                "| {tool} (h1) |  1 | {:>5.2} | {best:>5.2} | {:>5.2} |  | {} |",
                median(rates),
                median(cpus),
                loadavg()
            );
        }
    }
}

fn main() {
    let a = parse();
    if a.serve {
        serve(&a);
        return;
    }
    let mut child = None;
    let line = match &a.attach {
        Some(l) => l.clone(),
        None => {
            let mut c = Command::new(std::env::current_exe().unwrap())
                .args(["--serve", "--cache-mb", &a.cache_mb.to_string()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut line = String::new();
            BufReader::new(c.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            child = Some(c);
            line
        }
    };
    let parts: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(parts.first(), Some(&"READY"), "server said {line:?}");
    let cert = (0..parts[4].len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&parts[4][i..i + 2], 16).unwrap())
        .collect();
    let https_port = parts[2].rsplit(':').next().unwrap();
    let srv = Server {
        http: parts[1].into(),
        https: format!("localhost:{https_port}"),
        h3_port: parts[3].rsplit(':').next().unwrap().parse().unwrap(),
        cert,
    };
    println!(
        "{} on {} hardware threads, {} runs of {} GB each (median and best GB/s; \
         median client CPU seconds per GB), client runtime {} worker threads",
        a.file,
        std::thread::available_parallelism().map_or(0, |n| n.get()),
        a.runs,
        a.gb,
        a.threads
    );
    println!("| transport | conns | GB/s | best | CPU s/GB | KiB/chunk | load avg |");
    println!("|---|---:|---:|---:|---:|---:|---|");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(a.threads)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        for t in &a.transports {
            for &n in &a.conns {
                run(&a, &srv, t, n).await;
            }
        }
    });
    drop(rt);
    if a.compare {
        compare(&a, &srv);
    }
    if let Some(mut c) = child {
        drop(c.stdin.take());
        let _ = c.wait();
    }
}
