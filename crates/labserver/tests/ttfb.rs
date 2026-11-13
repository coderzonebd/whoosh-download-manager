//! Time to first byte under load.
//!
//! A multi-connection client opens ranges deep into a large file while its
//! first connection (and the others) stream at full speed. Those new ranges
//! must start about as fast as when the server is idle: if they wait behind
//! bulk transfers, a benchmark against `clean` measures labserver instead of
//! the client.
//!
//! The clients here are plain threads with blocking sockets, so they never
//! compete with the server for its tokio workers.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::thread;
use std::time::{Duration, Instant};

use whoosh_labserver::{LabConfig, LabServer, Scenario, VirtualFile};

const FILE: &str = "1g.bin";
const MIB: u64 = 1 << 20;
const SIZE: u64 = 1 << 30;
const PROBE_LEN: u64 = 64 * 1024;

fn get(addr: SocketAddr, start: u64, end_incl: u64) -> String {
    format!(
        "GET /s/clean/{FILE} HTTP/1.1\r\nHost: {addr}\r\nRange: bytes={start}-{end_incl}\r\n\r\n"
    )
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// New connection, one range request: the time from `connect` until the
/// first body byte, and the body bytes read so far.
fn probe(addr: SocketAddr, start: u64) -> (Duration, Vec<u8>) {
    let t0 = Instant::now();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_nodelay(true).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    s.write_all(get(addr, start, start + PROBE_LEN - 1).as_bytes())
        .unwrap();
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = Vec::new();
    loop {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "server closed before the first body byte");
        got.extend_from_slice(&buf[..n]);
        if let Some(p) = head_end(&got)
            && got.len() > p
        {
            let head = String::from_utf8_lossy(&got[..p]);
            assert!(head.starts_with("HTTP/1.1 206"), "{head}");
            return (t0.elapsed(), got[p..].to_vec());
        }
    }
}

/// Streams `[start, end)` over and over on one keep-alive connection,
/// discarding the bytes, until `stop`.
fn load(addr: SocketAddr, start: u64, end: u64, stop: &AtomicBool, bytes: &AtomicU64) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_nodelay(true).unwrap();
    let mut buf = vec![0u8; 256 * 1024];
    while !stop.load(Relaxed) {
        s.write_all(get(addr, start, end - 1).as_bytes()).unwrap();
        let mut head = Vec::new();
        let mut body = loop {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "server closed");
            head.extend_from_slice(&buf[..n]);
            if let Some(p) = head_end(&head) {
                break (head.len() - p) as u64;
            }
        };
        while body < end - start {
            if stop.load(Relaxed) {
                return;
            }
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "server closed");
            body += n as u64;
            bytes.fetch_add(n as u64, Relaxed);
        }
    }
}

fn quantile(v: &mut [Duration], q: f64) -> Duration {
    v.sort();
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

#[test]
fn deep_ranges_start_fast_under_concurrent_load() {
    let loaders: u64 = std::env::var("LAB_TTFB_LOADERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let samples: usize = std::env::var("LAB_TTFB_SAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25);

    // The server on its own runtime, as the bench runs it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let lab = rt
        .block_on(LabServer::start(LabConfig {
            tls_port: None,
            cache_bytes: 256 << 20,
            ..LabConfig::with_scenarios(vec![Scenario::clean("clean")])
        }))
        .unwrap();
    let addr = lab.http_addr();
    let file: VirtualFile = lab.file("clean", FILE).unwrap();
    assert_eq!(file.size, SIZE);

    let probe_round = |n: usize, idle: bool| {
        let (mut shallow, mut deep) = (Vec::new(), Vec::new());
        for i in 0..n as u64 {
            // Fresh offsets each time: nothing a probe asks for is cached
            // by an earlier probe.
            for (at, out) in [
                (i * MIB + 4096, &mut shallow),
                (900 * MIB + i * MIB + 4096, &mut deep),
            ] {
                let (t, body) = probe(addr, at);
                assert_eq!(file.verify(at, &body), Ok(()), "probe at {at}");
                out.push(t);
            }
            if !idle {
                thread::sleep(Duration::from_millis(10));
            }
        }
        (shallow, deep)
    };

    let (mut idle_shallow, mut idle_deep) = probe_round(samples.min(10), true);

    let stop = Arc::new(AtomicBool::new(false));
    let bytes = Arc::new(AtomicU64::new(0));
    let threads: Vec<_> = (0..loaders)
        .map(|k| {
            // Each loader owns a slice; together they cover the file, like a
            // multi-connection download.
            let (start, end) = (k * SIZE / loaders, (k + 1) * SIZE / loaders);
            let (stop, bytes) = (stop.clone(), bytes.clone());
            thread::spawn(move || load(addr, start, end, &stop, &bytes))
        })
        .collect();
    thread::sleep(Duration::from_millis(300));

    let t0 = Instant::now();
    let b0 = bytes.load(Relaxed);
    let (mut shallow, mut deep) = probe_round(samples, false);
    let rate = (bytes.load(Relaxed) - b0) as f64 / t0.elapsed().as_secs_f64();

    stop.store(true, Relaxed);
    for t in threads {
        t.join().unwrap();
    }

    let report = |name: &str, v: &mut Vec<Duration>| {
        let (p50, p90, max) = (quantile(v, 0.5), quantile(v, 0.9), quantile(v, 1.0));
        eprintln!(
            "{name:>22}: median {:7.2} ms  p90 {:7.2} ms  max {:7.2} ms",
            ms(p50),
            ms(p90),
            ms(max)
        );
        (p50, p90)
    };
    eprintln!(
        "{loaders} loaders streaming {:.2} GB/s; {} probes per offset",
        rate / 1e9,
        shallow.len()
    );
    let (i50, _) = report("idle, offset ~0", &mut idle_shallow);
    let (j50, _) = report("idle, offset ~900 MiB", &mut idle_deep);
    let (s50, s90) = report("load, offset ~0", &mut shallow);
    let (d50, d90) = report("load, offset ~900 MiB", &mut deep);

    // Deep ranges cost no more than shallow ones: content is generated
    // per block, wherever the block is.
    assert!(
        d50 <= s50 * 3 + Duration::from_millis(2),
        "deep median {d50:?} vs shallow {s50:?}"
    );
    // And no new request waits behind the bulk transfers. Bounds relative
    // to the idle time (~0.8 ms release, ~7 ms debug on the dev laptop) and
    // generous, for shared, loaded machines; without the scheduler yield in
    // `body.rs` the p90s were 125–2600 ms.
    let idle = i50.max(j50);
    let (max50, max90) = (
        idle * 4 + Duration::from_millis(15),
        idle * 8 + Duration::from_millis(50),
    );
    for (name, p50, p90) in [("shallow", s50, s90), ("deep", d50, d90)] {
        assert!(p50 <= max50, "{name} median {p50:?} > {max50:?}");
        assert!(p90 <= max90, "{name} p90 {p90:?} > {max90:?}");
    }
    drop(lab);
}
