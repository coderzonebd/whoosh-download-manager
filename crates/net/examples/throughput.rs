//! Throughput and CPU cost of `vayu-net` against labserver, over HTTP/1.1
//! (plain or TLS), HTTP/2 and HTTP/3, with 1, 4 and 16 concurrent opens.
//!
//! ```text
//! cargo run --release -p vayu-net --example throughput -- \
//!     [--transports h1,h1s,h2,h3] [--conns 1,4,16] [--file 100m.bin] \
//!     [--scenario clean | --rate <MB/s per connection>] \
//!     [--gb 1] [--runs 5] [--threads 4] [--cache-mb 256] [--compare]
//! ```
//!
//! Transports: `h1` plain HTTP/1.1, `h1s` HTTP/1.1 over TLS (`http2: false`,
//! like an HTTP/1.1-only HTTPS server), `h2` HTTP/2 over TLS, `h3` QUIC.
//!
//! The server runs in a child process (this binary with `--serve`), so the
//! CPU time measured here (`getrusage(RUSAGE_SELF)`) is the client's
//! alone. Each run moves `--gb` gigabytes: N workers each fetch their Nth of
//! the file repeatedly (one ranged request per pass, as the engine does),
//! on connections opened by an unmeasured warm-up pass. `--rate R` adds a
//! scenario `rated` capping every connection at R MB/s (slow arrival: small
//! reads, many wake-ups) and uses it.
//!
//! `--compare` runs other clients on the same scenario and connection
//! counts for the `h1`/`h1s` transports (their CPU from `RUSAGE_CHILDREN`):
//! `curl` as N processes each fetching its Nth with `-r` to `/dev/null`;
//! `aria2c -x N -s N -k 1M` (it writes the file: to `/dev/shm`); `wget` for
//! one connection.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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
    scenario: String,
    rate: Option<f64>,
    gb: f64,
    runs: usize,
    threads: usize,
    cache_mb: usize,
    compare: bool,
    /// Use a server started separately with `--serve` (its READY line).
    attach: Option<String>,
    /// `--serve`: write the certificate (PEM) here.
    write_pem: Option<PathBuf>,
}

fn parse() -> Args {
    let mut a = Args {
        serve: false,
        transports: vec!["h1".into(), "h2".into(), "h3".into()],
        conns: vec![1, 4, 16],
        file: "100m.bin".into(),
        scenario: "clean".into(),
        rate: None,
        gb: 1.0,
        runs: 5,
        threads: 4,
        cache_mb: 256,
        compare: false,
        attach: None,
        write_pem: None,
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
            "--scenario" => a.scenario = val(),
            "--rate" => {
                a.rate = Some(val().parse().unwrap());
                a.scenario = "rated".into();
            }
            "--gb" => a.gb = val().parse().unwrap(),
            "--runs" => a.runs = val().parse().unwrap(),
            "--threads" => a.threads = val().parse().unwrap(),
            "--cache-mb" => a.cache_mb = val().parse().unwrap(),
            "--attach" => a.attach = Some(val()),
            "--write-pem" => a.write_pem = Some(val().into()),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

/// `--serve`: labserver (built-in scenarios, plus `rated` with `--rate`,
/// HTTP/3 on) until stdin closes.
fn serve(a: &Args) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut cfg = vayu_labserver::LabConfig {
            h3_port: Some(0),
            cache_bytes: a.cache_mb << 20,
            ..Default::default()
        };
        if let Some(rate) = a.rate {
            let toml = format!("[bandwidth]\nper_conn = \"{rate}MB/s\"");
            cfg = cfg.scenario(vayu_labserver::Scenario::from_toml("rated", &toml).unwrap());
        }
        let lab = vayu_labserver::LabServer::start(cfg).await.unwrap();
        if let Some(p) = &a.write_pem {
            std::fs::write(p, lab.cert_pem().unwrap()).unwrap();
        }
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
    pem: Option<PathBuf>,
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

fn row(name: &str, n: u64, rates: Vec<f64>, cpus: Vec<f64>, chunk: Option<f64>) {
    let best = rates.iter().cloned().fold(0.0, f64::max);
    let chunk = chunk.map_or(String::new(), |c| format!("{c:.0}"));
    println!(
        "| {name} | {n:>2} | {:>6.3} | {best:>6.3} | {:>5.2} | {chunk:>5} | {} |",
        median(rates),
        median(cpus),
        loadavg()
    );
}

/// The `n` ranges a file is split into, as `(start, end)`.
fn slices(size: u64, n: u64) -> Vec<(u64, u64)> {
    let len = size / n;
    (0..n)
        .map(|i| (i * len, if i == n - 1 { size } else { (i + 1) * len }))
        .collect()
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
    let tasks: Vec<_> = slices(size, n)
        .into_iter()
        .map(|(a, b)| {
            let src = src.clone();
            let expect = expect.clone();
            let range = ByteRange::new(a, b).unwrap();
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

fn url_for(a: &Args, srv: &Server, transport: &str) -> String {
    let path = format!("/s/{}/{}", a.scenario, a.file);
    match transport {
        "h1" => format!("http://{}{path}", srv.http),
        _ => format!("https://{}{path}", srv.https),
    }
}

async fn run(a: &Args, srv: &Server, transport: &str, n: u64) -> u64 {
    let cfg = NetConfig {
        extra_root_certs: vec![srv.cert.clone()],
        platform_roots: false,
        h3_fallback_delay: Duration::from_secs(5),
        // The server advertises HTTP/3; only the h3 rows may follow it.
        http3: transport == "h3",
        http2: transport != "h1s",
        ..NetConfig::default()
    };
    let factory = HttpSourceFactory::new(cfg);
    let want = match transport {
        "h1" | "h1s" => Protocol::Http1,
        "h2" => Protocol::Http2,
        "h3" => {
            let origin = format!("https://{}/", srv.https);
            factory.remember_http3(&origin, srv.h3_port, Duration::from_secs(3600));
            Protocol::Http3
        }
        t => panic!("unknown transport {t}"),
    };
    let src = factory.source(&url_for(a, srv, transport), None).unwrap();
    let meta = src.probe().await.unwrap();
    assert_eq!(meta.protocol, Some(want), "{transport}");
    let size = meta.size.unwrap();
    let expect = meta.validators();
    let reps = ((a.gb * 1e9) / size as f64).ceil().max(1.0) as u64;
    let _ = pass(&src, size, n, 1, &expect).await; // warm-up: connections, caches
    let (mut rates, mut cpus, mut chunk) = (Vec::new(), Vec::new(), 0.0);
    for _ in 0..a.runs {
        let (c0, t0) = (cpu(false), Instant::now());
        let (bytes, chunks) = pass(&src, size, n, reps, &expect).await;
        let (dt, dc) = (t0.elapsed().as_secs_f64(), cpu(false) - c0);
        assert_eq!(bytes, size * reps);
        chunk = bytes as f64 / chunks as f64 / 1024.0;
        rates.push(bytes as f64 / dt / 1e9);
        cpus.push(dc / (bytes as f64 / 1e9));
    }
    row(
        &format!("vayu-net {transport}"),
        n,
        rates,
        cpus,
        Some(chunk),
    );
    size
}

/// Runs `make()` `runs` times; rows of GB/s and CPU s/GB for `gb` per run.
fn tool_row(name: &str, n: u64, runs: usize, gb: f64, mut make: impl FnMut() -> Vec<Command>) {
    let (mut rates, mut cpus) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        let (c0, t0) = (cpu(true), Instant::now());
        let children: Vec<_> = make()
            .iter_mut()
            .filter_map(|c| c.stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok())
            .collect();
        let ok = !children.is_empty()
            && children
                .into_iter()
                .all(|mut c| c.wait().is_ok_and(|s| s.success()));
        let (dt, dc) = (t0.elapsed().as_secs_f64(), cpu(true) - c0);
        if !ok {
            println!("| {name} | {n:>2} | failed | | | | |");
            return;
        }
        rates.push(gb / dt);
        cpus.push(dc / gb);
    }
    row(name, n, rates, cpus, None);
}

fn compare(a: &Args, srv: &Server, transport: &str, n: u64, size: u64) {
    let url = url_for(a, srv, transport);
    let reps = ((a.gb * 1e9) / size as f64).ceil().max(1.0) as usize;
    let gb = (size as usize * reps) as f64 / 1e9;
    let pem = srv
        .pem
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let tls = transport != "h1";
    // curl: N processes, each fetching its slice `reps` times (one
    // connection each, kept alive across its URLs).
    tool_row(&format!("curl {transport}"), n, a.runs, gb, || {
        slices(size, n)
            .into_iter()
            .map(|(s, e)| {
                let mut c = Command::new("curl");
                c.args(["-s", "--http1.1", "-o", "/dev/null"]);
                if tls {
                    c.args(["--cacert", &pem]);
                }
                for _ in 0..reps {
                    c.args(["-r", &format!("{s}-{}", e - 1), &url]);
                    c.args(["-o", "/dev/null"]);
                }
                c
            })
            .collect()
    });
    if n == 1 {
        tool_row(&format!("wget {transport}"), n, a.runs, gb, || {
            let mut c = Command::new("wget");
            c.args(["-q", "-O", "/dev/null"]);
            if tls {
                c.args(["--ca-certificate", &pem]);
            }
            c.args(vec![url.as_str(); reps]);
            vec![c]
        });
    }
    // aria2c writes the file; /dev/shm keeps the disk out of it.
    let dir = std::env::temp_dir().join(format!("vayu-throughput-{}", std::process::id()));
    let shm = PathBuf::from("/dev/shm").join(dir.file_name().unwrap());
    let out = if PathBuf::from("/dev/shm").is_dir() {
        shm
    } else {
        dir
    };
    let _ = std::fs::create_dir_all(&out);
    tool_row(&format!("aria2c {transport}"), n, a.runs, gb, || {
        let mut c = Command::new("sh");
        let mut script = String::new();
        for _ in 0..reps {
            script.push_str(&format!(
                "aria2c -q -x {n} -s {n} -k 1M --min-split-size=1M --file-allocation=none \
                 --allow-overwrite=true --auto-file-renaming=false {} -d '{}' -o f '{url}' && rm -f '{}/f' && ",
                if tls { format!("--ca-certificate='{pem}'") } else { String::new() },
                out.display(),
                out.display()
            ));
        }
        script.push_str("true");
        c.args(["-c", &script]);
        vec![c]
    });
    let _ = std::fs::remove_dir_all(&out);
}

fn main() {
    let a = parse();
    if a.serve {
        serve(&a);
        return;
    }
    let pem = std::env::temp_dir().join(format!("vayu-throughput-{}.pem", std::process::id()));
    let mut child = None;
    let line = match &a.attach {
        Some(l) => l.clone(),
        None => {
            let mut args = vec![
                "--serve".to_string(),
                "--cache-mb".into(),
                a.cache_mb.to_string(),
                "--write-pem".into(),
                pem.display().to_string(),
            ];
            if let Some(r) = a.rate {
                args.extend(["--rate".into(), r.to_string()]);
            }
            let mut c = Command::new(std::env::current_exe().unwrap())
                .args(args)
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
        pem: pem.exists().then_some(pem.clone()),
    };
    println!(
        "{}/{} on {} hardware threads, {} runs of {} GB each (median and best GB/s; \
         median client CPU seconds per GB; KiB per chunk handed over), \
         client runtime {} worker threads",
        a.scenario,
        a.file,
        std::thread::available_parallelism().map_or(0, |n| n.get()),
        a.runs,
        a.gb,
        a.threads
    );
    println!("| client | conns | GB/s | best | CPU s/GB | KiB/chunk | load avg |");
    println!("|---|---:|---:|---:|---:|---:|---|");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(a.threads)
        .enable_all()
        .build()
        .unwrap();
    for t in &a.transports {
        for &n in &a.conns {
            let size = rt.block_on(run(&a, &srv, t, n));
            if a.compare && (t == "h1" || t == "h1s") {
                compare(&a, &srv, t, n, size);
            }
        }
    }
    drop(rt);
    if let Some(mut c) = child {
        drop(c.stdin.take());
        let _ = c.wait();
    }
    let _ = std::fs::remove_file(&pem);
}
