//! How fast can the labserver serve on loopback?
//!
//! Starts a server in-process with one fault-free scenario and runs N client
//! connections that each fetch their own slice of a file over and over,
//! discarding the bytes. Build it in release mode:
//!
//! ```text
//! cargo run --release -p vayu-labserver --example throughput -- --conns 4 --secs 5
//! cargo run --release -p vayu-labserver --example throughput -- --file 1g.bin --cache-mb 0   # generation-bound
//! cargo run --release -p vayu-labserver --example throughput -- --h2 --tls
//! ```

use bytes::Bytes;
use clap::Parser;
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::{CertificateDer, ServerName};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use vayu_labserver::{LabConfig, LabServer, Scenario};

#[derive(Debug, Parser)]
struct Args {
    /// Parallel client connections.
    #[arg(long, default_value_t = 4)]
    conns: usize,
    /// Measurement time.
    #[arg(long, default_value_t = 5.0)]
    secs: f64,
    /// File from the default set (empty/1m/10m/100m/1g/10g.bin).
    #[arg(long, default_value = "100m.bin")]
    file: String,
    /// Block cache size; 0 forces every block to be generated per request.
    #[arg(long, default_value_t = 256)]
    cache_mb: usize,
    /// Use HTTP/2 (one stream per connection).
    #[arg(long)]
    h2: bool,
    /// Use TLS.
    #[arg(long)]
    tls: bool,
    /// Plain-socket HTTP/1.1 client (no hyper on the client side), reading
    /// into a cache-sized 256 KiB buffer: measures the server, not the client.
    #[arg(long, conflicts_with_all = ["h2", "tls"])]
    raw: bool,
    /// Tokio worker threads (client and server share the runtime); 0 = one per CPU.
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

enum Sender {
    H1(http1::SendRequest<Empty<Bytes>>),
    H2(http2::SendRequest<Empty<Bytes>>),
}

async fn handshake<T>(io: T, h2: bool) -> Sender
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(io);
    if h2 {
        let mut b = http2::Builder::new(TokioExecutor::new());
        b.initial_stream_window_size(16 << 20)
            .initial_connection_window_size(32 << 20)
            .max_frame_size(1 << 20);
        let (s, c) = b.handshake(io).await.unwrap();
        tokio::spawn(c);
        Sender::H2(s)
    } else {
        let (s, c) = http1::handshake(io).await.unwrap();
        tokio::spawn(c);
        Sender::H1(s)
    }
}

async fn connect(addr: SocketAddr, args: &Args, cert: Option<&[u8]>) -> Sender {
    let tcp = TcpStream::connect(addr).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    match cert {
        None => handshake(tcp, args.h2).await,
        Some(der) => {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(CertificateDer::from(der.to_vec())).unwrap();
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            cfg.alpn_protocols = vec![if args.h2 {
                b"h2".to_vec()
            } else {
                b"http/1.1".to_vec()
            }];
            let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
                .connect(ServerName::try_from("localhost").unwrap(), tcp)
                .await
                .unwrap();
            handshake(tls, args.h2).await
        }
    }
}

/// One keep-alive connection, hand-written HTTP/1.1, body bytes discarded.
async fn raw_worker(
    addr: SocketAddr,
    args: Arc<Args>,
    slice: (u64, u64),
    deadline: Instant,
) -> u64 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    let req = format!(
        "GET /s/clean/{} HTTP/1.1\r\nHost: localhost\r\nRange: bytes={}-{}\r\n\r\n",
        args.file,
        slice.0,
        slice.1 - 1
    );
    let want = slice.1 - slice.0;
    let mut buf = vec![0u8; 256 << 10];
    let mut total = 0;
    while Instant::now() < deadline {
        tcp.write_all(req.as_bytes()).await.unwrap();
        // Read the head (it may arrive with the first body bytes).
        let mut head = Vec::new();
        let body_start = loop {
            let n = tcp.read(&mut buf).await.unwrap();
            assert!(n > 0, "server closed");
            head.extend_from_slice(&buf[..n]);
            if let Some(p) = head.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
        };
        let mut got = (head.len() - body_start) as u64;
        while got < want {
            let n = tcp.read(&mut buf).await.unwrap();
            assert!(n > 0, "server closed");
            got += n as u64;
            if Instant::now() >= deadline {
                return total + got;
            }
        }
        total += got;
    }
    total
}

async fn worker(
    addr: SocketAddr,
    args: Arc<Args>,
    cert: Option<Vec<u8>>,
    slice: (u64, u64),
    deadline: Instant,
) -> u64 {
    let mut s = connect(addr, &args, cert.as_deref()).await;
    let scheme = if args.tls { "https" } else { "http" };
    let mut total = 0;
    while Instant::now() < deadline {
        let req = http::Request::get(format!(
            "{scheme}://localhost:{}/s/clean/{}",
            addr.port(),
            args.file
        ))
        .header("host", format!("localhost:{}", addr.port()))
        .header("range", format!("bytes={}-{}", slice.0, slice.1 - 1))
        .body(Empty::new())
        .unwrap();
        let resp = match &mut s {
            Sender::H1(s) => {
                s.ready().await.unwrap();
                s.send_request(req).await.unwrap()
            }
            Sender::H2(s) => {
                s.ready().await.unwrap();
                s.send_request(req).await.unwrap()
            }
        };
        let mut body = resp.into_body();
        while let Some(f) = body.frame().await {
            if let Some(d) = f.unwrap().data_ref() {
                total += d.len() as u64;
            }
            if Instant::now() >= deadline {
                return total;
            }
        }
    }
    total
}

fn main() {
    let args = Arc::new(Args::parse());
    let mut rt = tokio::runtime::Builder::new_multi_thread();
    if args.threads > 0 {
        rt.worker_threads(args.threads);
    }
    rt.enable_all().build().unwrap().block_on(run(args));
}

async fn run(args: Arc<Args>) {
    let lab = LabServer::start(LabConfig {
        scenarios: vec![Scenario::clean("clean")],
        cache_bytes: args.cache_mb << 20,
        tls_port: args.tls.then_some(0),
        ..LabConfig::default()
    })
    .await
    .unwrap();
    let addr = if args.tls {
        lab.https_addr().unwrap()
    } else {
        lab.http_addr()
    };
    let size = lab.file("clean", &args.file).expect("unknown file").size;
    let cert = lab.cert_der().map(<[u8]>::to_vec);
    let n = args.conns as u64;

    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs_f64(args.secs);
    let tasks: Vec<_> = (0..n)
        .map(|i| {
            let slice = (i * size / n, (i + 1) * size / n);
            if args.raw {
                tokio::spawn(raw_worker(addr, args.clone(), slice, deadline))
            } else {
                tokio::spawn(worker(addr, args.clone(), cert.clone(), slice, deadline))
            }
        })
        .collect();
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    let secs = t0.elapsed().as_secs_f64();
    let served = lab.stats("clean").unwrap().bytes_sent;
    println!(
        "{} conns, {}{}, {} (cache {} MiB): client {:.2} GB/s ({:.2} GiB/s), server handed out {:.2} GB/s over {:.1} s",
        n,
        if args.raw {
            "raw http/1.1"
        } else if args.h2 {
            "h2"
        } else {
            "http/1.1"
        },
        if args.tls { "+tls" } else { "" },
        args.file,
        args.cache_mb,
        total as f64 / secs / 1e9,
        total as f64 / secs / (1u64 << 30) as f64,
        served as f64 / secs / 1e9,
        secs
    );
}
